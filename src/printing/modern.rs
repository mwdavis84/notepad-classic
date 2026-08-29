//! Windows.Graphics.Printing document source for the native system print UI.
//!
//! The system owns the print UI and calls this object through three interfaces:
//! `IPrintDocumentSource` identifies the WinRT source, while the two classic COM
//! interfaces provide pagination/preview pages and the final print package. The
//! object owns an immutable document snapshot and serializes callbacks so no
//! application state is borrowed while the print UI is reentrant.

use std::cell::Cell;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Printing::{
    IPrintDocumentSource, IPrintDocumentSource_Impl, IPrintTaskOptionsCore, PrintManager,
    PrintPageDescription, PrintTask, PrintTaskCompletedEventArgs, PrintTaskCompletion,
    PrintTaskOptions, PrintTaskRequestedEventArgs, PrintTaskSourceRequestedHandler,
    StandardPrintTaskOptions,
};
use windows::Win32::Foundation::{
    E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, ERROR_NOT_SUPPORTED, HWND as WindowsHwnd,
    REGDB_E_CLASSNOTREG, RO_E_METADATA_NAME_NOT_FOUND,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_COLOR_SPACE_SRGB, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_PRINT_CONTROL_PROPERTIES,
    D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT, D2D1CreateDevice, ID2D1Device, ID2D1DeviceContext,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::Graphics::Printing::{FinalPageCount, IPrintPreviewDxgiPackageTarget};
use windows::Win32::Storage::Xps::Printing::IPrintDocumentPackageTarget;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IStream};
use windows::Win32::System::WinRT::Printing::{
    IPrintDocumentPageSource, IPrintDocumentPageSource_Impl, IPrintManagerInterop,
    IPrintPreviewPageCollection, IPrintPreviewPageCollection_Impl,
};
use windows::Win32::System::WinRT::{
    RO_INIT_SINGLETHREADED, RoGetActivationFactory, RoInitialize, RoUninitialize,
};
use windows::core::{
    AgileReference, Error, HRESULT, HSTRING, IUnknownImpl, Interface, PCWSTR, Ref,
    Result as WindowsResult,
};
use windows_future::IAsyncOperation;
use windows_numerics::{Matrix3x2, Vector2};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
use windows_sys::Win32::System::SystemServices::LOCALE_NAME_MAX_LENGTH;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

use crate::app::FontChoice;

use super::{AsyncPrintFailure, PaginatedText, paginate, post_async_failure};

const PRINT_MANAGER_CLASS: &str = "Windows.Graphics.Printing.PrintManager";
const JOB_PAGE_APPLICATION_DEFINED: u32 = u32::MAX;
const PRINT_MARGIN_DIP: f32 = 24.0;
const LAYOUT_SCALE: f32 = 1_000.0;

thread_local! {
    // RoInitialize is deliberately deferred until the first Print command. A
    // successful call (including S_FALSE) must be balanced exactly once.
    static WINRT_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

static ACTIVE_PRINT_WINDOWS: LazyLock<Mutex<HashSet<isize>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Debug)]
pub(super) enum ModernPrintError {
    Unavailable,
    Failed(Error),
}

struct DocumentSnapshot {
    text: Mutex<Arc<Vec<u16>>>,
    font: FontChoice,
    display_name: String,
    preview_dpi: u32,
}

impl DocumentSnapshot {
    fn text(&self) -> WindowsResult<Arc<Vec<u16>>> {
        self.text
            .lock()
            .map(|text| Arc::clone(&text))
            .map_err(|_| Error::from_hresult(E_FAIL))
    }

    fn make_text_authoritative(&self, text: Arc<Vec<u16>>) -> WindowsResult<()> {
        let mut current = self.text.lock().map_err(|_| Error::from_hresult(E_FAIL))?;
        if !Arc::ptr_eq(&current, &text) {
            *current = text;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ModernPageGeometry {
    page_width: f32,
    page_height: f32,
    content_left: f32,
    content_top: f32,
    content_width: f32,
    content_height: f32,
    dpi_x: u32,
    dpi_y: u32,
}

struct PrintLayout {
    geometry: ModernPageGeometry,
    line_height: f32,
    document: PaginatedText,
}

struct GraphicsResources {
    d3d_device: ID3D11Device,
    d2d_device: ID2D1Device,
    dwrite_factory: IDWriteFactory,
    text_format: IDWriteTextFormat,
    underline: bool,
    strikeout: bool,
}

struct DocumentState {
    snapshot: Option<Arc<DocumentSnapshot>>,
    preview_target: Option<AgileReference<IPrintPreviewDxgiPackageTarget>>,
    graphics: Option<Arc<GraphicsResources>>,
    layout: Option<Arc<PrintLayout>>,
}

impl DocumentState {
    fn new(snapshot: Arc<DocumentSnapshot>) -> Self {
        Self {
            snapshot: Some(snapshot),
            preview_target: None,
            graphics: None,
            layout: None,
        }
    }
}

#[windows::core::implement(
    IPrintDocumentSource,
    IPrintDocumentPageSource,
    IPrintPreviewPageCollection
)]
struct DocumentSource {
    state: Arc<Mutex<DocumentState>>,
}

impl DocumentSource {
    fn new(state: Arc<Mutex<DocumentState>>) -> Self {
        Self { state }
    }

    fn state(&self) -> WindowsResult<MutexGuard<'_, DocumentState>> {
        self.state.lock().map_err(|_| Error::from_hresult(E_FAIL))
    }

    fn snapshot(&self) -> WindowsResult<Arc<DocumentSnapshot>> {
        self.state()?
            .snapshot
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| Error::from_hresult(E_FAIL))
    }

    fn ensure_graphics(
        &self,
        state: &mut DocumentState,
        font: FontChoice,
    ) -> WindowsResult<Arc<GraphicsResources>> {
        if state.graphics.is_none() {
            state.graphics = Some(Arc::new(GraphicsResources::new(font)?));
        }
        Ok(Arc::clone(
            state.graphics.as_ref().expect("graphics was initialized"),
        ))
    }

    fn layout_for_options(
        &self,
        options: &windows::core::IInspectable,
    ) -> WindowsResult<Arc<PrintLayout>> {
        let snapshot = self.snapshot()?;
        let options: IPrintTaskOptionsCore = options.cast()?;
        // This document uses one uniform layout for every page. Page zero is
        // the generic description used by Microsoft's current Printing sample;
        // per-page descriptions are unnecessary until page-specific tickets or
        // layouts are supported here.
        let description = options.GetPageDescription(0)?;
        let geometry = geometry_from_page_description(description)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let graphics = {
            let mut state = self.state()?;
            if let Some(layout) = &state.layout {
                if layout.geometry == geometry {
                    return Ok(Arc::clone(layout));
                }
            }
            self.ensure_graphics(&mut state, snapshot.font)?
        };
        let layout = Arc::new(create_layout(&snapshot, &graphics, geometry)?);
        self.state()?.layout = Some(Arc::clone(&layout));
        Ok(layout)
    }
}

impl IPrintDocumentSource_Impl for DocumentSource_Impl {}

impl IPrintDocumentPageSource_Impl for DocumentSource_Impl {
    fn GetPreviewPageCollection(
        &self,
        document_target: Ref<IPrintDocumentPackageTarget>,
    ) -> WindowsResult<IPrintPreviewPageCollection> {
        let target = document_target.ok()?;
        let preview = unsafe {
            target.GetPackageTarget::<IPrintPreviewDxgiPackageTarget>(
                &IPrintPreviewDxgiPackageTarget::IID,
            )?
        };
        self.state()?.preview_target = Some(AgileReference::new(&preview)?);
        Ok(self.to_interface())
    }

    fn MakeDocument(
        &self,
        print_task_options: Ref<windows::core::IInspectable>,
        document_target: Ref<IPrintDocumentPackageTarget>,
    ) -> WindowsResult<()> {
        let options = print_task_options.ok()?;
        let target = document_target.ok()?;
        let layout = self.layout_for_options(options)?;
        let page_indices = selected_page_indices(options, layout.document.pages.len())?;
        let graphics = {
            let mut state = self.state()?;
            let font = state
                .snapshot
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?
                .font;
            self.ensure_graphics(&mut state, font)?
        };
        graphics.print_document(target, &layout, &page_indices)
    }
}

impl IPrintPreviewPageCollection_Impl for DocumentSource_Impl {
    fn Paginate(
        &self,
        _current_job_page: u32,
        print_task_options: Ref<windows::core::IInspectable>,
    ) -> WindowsResult<()> {
        let options = print_task_options.ok()?;
        let layout = self.layout_for_options(options)?;
        let preview = {
            let state = self.state()?;
            state
                .preview_target
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?
                .resolve()?
        };
        unsafe {
            preview.InvalidatePreview()?;
            preview.SetJobPageCount(FinalPageCount, layout.document.pages.len() as u32)?;
        }
        Ok(())
    }

    fn MakePage(&self, desired_job_page: u32, width: f32, height: f32) -> WindowsResult<()> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }

        let page_number = if desired_job_page == JOB_PAGE_APPLICATION_DEFINED {
            1
        } else {
            desired_job_page
        };
        let snapshot = self.snapshot()?;
        let (layout, preview, graphics) = {
            let mut state = self.state()?;
            let layout = state
                .layout
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| Error::from_hresult(E_FAIL))?;
            let preview = state
                .preview_target
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?;
            let preview = preview.resolve()?;
            let graphics = self.ensure_graphics(&mut state, snapshot.font)?;
            (layout, preview, graphics)
        };
        if page_number == 0 || page_number as usize > layout.document.pages.len() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        graphics.draw_preview(
            &preview,
            &layout,
            page_number,
            width,
            height,
            snapshot.preview_dpi,
        )
    }
}

impl GraphicsResources {
    fn new(font: FontChoice) -> WindowsResult<Self> {
        let d3d_device = create_d3d_device()?;
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let d2d_device = unsafe { D2D1CreateDevice(&dxgi_device, None)? };
        let dwrite_factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let text_format = create_text_format(&dwrite_factory, font)?;
        Ok(Self {
            d3d_device,
            d2d_device,
            dwrite_factory,
            text_format,
            underline: font.logical.lfUnderline != 0,
            strikeout: font.logical.lfStrikeOut != 0,
        })
    }

    fn text_layout(
        &self,
        text: &[u16],
        max_width: f32,
        max_height: f32,
    ) -> WindowsResult<IDWriteTextLayout> {
        let layout = unsafe {
            self.dwrite_factory.CreateTextLayout(
                text,
                &self.text_format,
                max_width.max(1.0),
                max_height.max(1.0),
            )?
        };
        if !text.is_empty() && (self.underline || self.strikeout) {
            let range = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: text.len() as u32,
            };
            unsafe {
                if self.underline {
                    layout.SetUnderline(true, range)?;
                }
                if self.strikeout {
                    layout.SetStrikethrough(true, range)?;
                }
            }
        }
        Ok(layout)
    }

    fn measure_width(&self, text: &[u16]) -> WindowsResult<f32> {
        if text.is_empty() {
            return Ok(0.0);
        }
        let layout = self.text_layout(text, 1_000_000.0, 10_000.0)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics.widthIncludingTrailingWhitespace.max(0.0))
    }

    fn line_height(&self) -> WindowsResult<f32> {
        let sample: Vec<u16> = "Mg".encode_utf16().collect();
        let layout = self.text_layout(&sample, 10_000.0, 10_000.0)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        Ok(metrics.height.max(1.0))
    }

    fn draw_page_contents(
        &self,
        context: &ID2D1DeviceContext,
        layout: &PrintLayout,
        page_index: usize,
    ) -> WindowsResult<()> {
        let white = D2D1_COLOR_F {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        let black = D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        unsafe { context.Clear(Some(&white)) };
        let brush = unsafe { context.CreateSolidColorBrush(&black, None)? };
        let page = layout
            .document
            .pages
            .get(page_index)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;

        for (line_index, line) in page.lines.iter().enumerate() {
            let line = layout.document.line(*line);
            if line.is_empty() {
                continue;
            }
            let text_layout = self.text_layout(
                line,
                layout.geometry.content_width,
                layout.line_height * 2.0,
            )?;
            let origin = Vector2 {
                X: layout.geometry.content_left,
                Y: layout.geometry.content_top + line_index as f32 * layout.line_height,
            };
            unsafe {
                context.DrawTextLayout(origin, &text_layout, &brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
            }
        }
        Ok(())
    }

    fn draw_preview(
        &self,
        preview: &IPrintPreviewDxgiPackageTarget,
        layout: &PrintLayout,
        page_number: u32,
        desired_width: f32,
        desired_height: f32,
        preview_dpi: u32,
    ) -> WindowsResult<()> {
        let context = unsafe {
            self.d2d_device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?
        };
        let preview_dpi = preview_dpi.max(96) as f32;
        let pixel_width = checked_surface_dimension(desired_width * preview_dpi / 96.0)?;
        let pixel_height = checked_surface_dimension(desired_height * preview_dpi / 96.0)?;
        let description = D3D11_TEXTURE2D_DESC {
            Width: pixel_width,
            Height: pixel_height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            self.d3d_device
                .CreateTexture2D(&description, None, Some(&mut texture))?;
        }
        let texture = texture.ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let surface: IDXGISurface = texture.cast()?;

        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: preview_dpi,
            dpiY: preview_dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS(
                D2D1_BITMAP_OPTIONS_TARGET.0 | D2D1_BITMAP_OPTIONS_CANNOT_DRAW.0,
            ),
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let target = unsafe { context.CreateBitmapFromDxgiSurface(&surface, Some(&properties))? };
        unsafe {
            context.SetTarget(&target);
            let scale = (desired_width / layout.geometry.page_width)
                .min(desired_height / layout.geometry.page_height);
            context.SetTransform(&Matrix3x2::scale(scale, scale));
            context.BeginDraw();
        }
        let draw_result = self.draw_page_contents(&context, layout, (page_number - 1) as usize);
        let end_result = unsafe { context.EndDraw(None, None) };
        unsafe {
            context.SetTarget(None::<&windows::Win32::Graphics::Direct2D::ID2D1Image>);
        }
        draw_result?;
        end_result?;
        unsafe { preview.DrawPage(page_number, &surface, preview_dpi, preview_dpi) }
    }

    fn print_document(
        &self,
        target: &IPrintDocumentPackageTarget,
        layout: &PrintLayout,
        page_indices: &[usize],
    ) -> WindowsResult<()> {
        let context = unsafe {
            self.d2d_device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?
        };
        let properties = D2D1_PRINT_CONTROL_PROPERTIES {
            fontSubset: D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT,
            rasterDPI: layout.geometry.dpi_x.min(layout.geometry.dpi_y).max(96) as f32,
            colorSpace: D2D1_COLOR_SPACE_SRGB,
        };
        // IWIC's factory projection is apartment-bound in windows-rs. Create
        // and consume it within this callback rather than sharing it through
        // the agile document source.
        let wic_factory: IWICImagingFactory = unsafe {
            CoCreateInstance(
                &CLSID_WICImagingFactory,
                None::<&windows::core::IUnknown>,
                CLSCTX_INPROC_SERVER,
            )?
        };
        let print_control = unsafe {
            self.d2d_device
                .CreatePrintControl(&wic_factory, target, Some(&properties))?
        };

        let render_result: WindowsResult<()> = (|| {
            for &page_index in page_indices {
                let command_list = unsafe { context.CreateCommandList()? };
                unsafe {
                    context.SetTarget(&command_list);
                    context.BeginDraw();
                }
                let draw_result = self.draw_page_contents(&context, layout, page_index);
                let end_result = unsafe { context.EndDraw(None, None) };
                unsafe {
                    context.SetTarget(None::<&windows::Win32::Graphics::Direct2D::ID2D1Image>);
                }
                draw_result?;
                end_result?;
                unsafe { command_list.Close()? };
                unsafe {
                    print_control.AddPage(
                        &command_list,
                        D2D_SIZE_F {
                            width: layout.geometry.page_width,
                            height: layout.geometry.page_height,
                        },
                        None::<&IStream>,
                        None,
                        None,
                    )?;
                }
            }
            Ok(())
        })();

        // A print control maps to exactly one print job and must be closed even
        // when recording or adding a page fails.
        let close_result = unsafe { print_control.Close() };
        render_result?;
        close_result
    }
}

fn create_d3d_device() -> WindowsResult<ID3D11Device> {
    fn create(
        driver_type: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE,
    ) -> WindowsResult<ID3D11Device> {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
                driver_type,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
        }
        device.ok_or_else(|| Error::from_hresult(E_FAIL))
    }

    create(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| create(D3D_DRIVER_TYPE_WARP))
}

fn create_text_format(
    factory: &IDWriteFactory,
    font: FontChoice,
) -> WindowsResult<IDWriteTextFormat> {
    let face_end = font
        .logical
        .lfFaceName
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(font.logical.lfFaceName.len());
    let mut face = font.logical.lfFaceName[..face_end].to_vec();
    if face.is_empty() {
        face.extend("Consolas".encode_utf16());
    }
    face.push(0);
    let locale = user_locale_name();
    let point_size = (font.point_size_tenths.max(1) as f32 / 10.0) * (96.0 / 72.0);
    let style = if font.logical.lfItalic != 0 {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    };
    let format = unsafe {
        factory.CreateTextFormat(
            PCWSTR(face.as_ptr()),
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT(if font.logical.lfWeight > 0 {
                font.logical.lfWeight.min(999)
            } else {
                400
            }),
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            point_size,
            PCWSTR(locale.as_ptr()),
        )?
    };
    unsafe { format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)? };
    Ok(format)
}

fn user_locale_name() -> Vec<u16> {
    let mut locale = vec![0u16; LOCALE_NAME_MAX_LENGTH as usize];
    let length = unsafe { GetUserDefaultLocaleName(locale.as_mut_ptr(), locale.len() as i32) };
    if length > 0 {
        locale.truncate(length as usize);
        locale
    } else {
        "en-US\0".encode_utf16().collect()
    }
}

fn create_layout(
    snapshot: &DocumentSnapshot,
    graphics: &GraphicsResources,
    geometry: ModernPageGeometry,
) -> WindowsResult<PrintLayout> {
    let line_height = graphics.line_height()?;
    let max_width = (geometry.content_width * LAYOUT_SCALE).round().max(1.0) as i32;
    let lines_per_page = (geometry.content_height / line_height).floor().max(1.0) as usize;
    let document = paginate(snapshot.text()?, max_width, lines_per_page, |text| {
        graphics
            .measure_width(text)
            .ok()
            .map(|width| (width * LAYOUT_SCALE).round().max(0.0) as i32)
    })
    .ok_or_else(|| Error::from_hresult(E_FAIL))?;
    // Tab expansion makes the paginated backing the canonical immutable text.
    // Reusing it for later geometry changes releases the original snapshot and
    // avoids retaining two document-sized allocations throughout the session.
    snapshot.make_text_authoritative(Arc::clone(&document.text))?;
    Ok(PrintLayout {
        geometry,
        line_height,
        document,
    })
}

fn geometry_from_page_description(description: PrintPageDescription) -> Option<ModernPageGeometry> {
    let page_width = description.PageSize.Width;
    let page_height = description.PageSize.Height;
    if !page_width.is_finite()
        || !page_height.is_finite()
        || page_width <= 0.0
        || page_height <= 0.0
    {
        return None;
    }

    let rect = description.ImageableRect;
    let mut left = rect.X.max(0.0).min(page_width);
    let mut top = rect.Y.max(0.0).min(page_height);
    let mut right = (rect.X + rect.Width).max(left).min(page_width);
    let mut bottom = (rect.Y + rect.Height).max(top).min(page_height);
    if !left.is_finite()
        || !top.is_finite()
        || !right.is_finite()
        || !bottom.is_finite()
        || right <= left
        || bottom <= top
    {
        left = 0.0;
        top = 0.0;
        right = page_width;
        bottom = page_height;
    }

    let imageable_width = right - left;
    let imageable_height = bottom - top;
    let inset_x = PRINT_MARGIN_DIP.min((imageable_width - 1.0).max(0.0) / 2.0);
    let inset_y = PRINT_MARGIN_DIP.min((imageable_height - 1.0).max(0.0) / 2.0);
    Some(ModernPageGeometry {
        page_width,
        page_height,
        content_left: left + inset_x,
        content_top: top + inset_y,
        content_width: (imageable_width - 2.0 * inset_x).max(1.0),
        content_height: (imageable_height - 2.0 * inset_y).max(1.0),
        dpi_x: description.DpiX.max(1),
        dpi_y: description.DpiY.max(1),
    })
}

fn checked_surface_dimension(value: f32) -> WindowsResult<u32> {
    if !value.is_finite() || value <= 0.0 || value > u16::MAX as f32 {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    Ok(value.ceil().max(1.0) as u32)
}

fn selected_page_indices(
    options: &windows::core::IInspectable,
    page_count: usize,
) -> WindowsResult<Vec<usize>> {
    let Ok(options) = options.cast::<PrintTaskOptions>() else {
        return Ok((0..page_count).collect());
    };
    let Ok(ranges) = options.CustomPageRanges() else {
        return Ok((0..page_count).collect());
    };
    let range_count = ranges.Size()?;
    if range_count == 0 {
        return Ok((0..page_count).collect());
    }

    let mut requested_ranges = Vec::with_capacity(range_count as usize);
    for index in 0..range_count {
        let range = ranges.GetAt(index)?;
        requested_ranges.push((range.FirstPageNumber()?, range.LastPageNumber()?));
    }
    let selected = normalize_page_ranges(&requested_ranges, page_count);
    if selected.is_empty() {
        Err(Error::from_hresult(E_INVALIDARG))
    } else {
        Ok(selected)
    }
}

fn normalize_page_ranges(ranges: &[(i32, i32)], page_count: usize) -> Vec<usize> {
    let mut selected = vec![false; page_count];
    for &(first, last) in ranges {
        let first = first.max(1) as usize;
        let last = last.max(0) as usize;
        if first > last || first > page_count {
            continue;
        }
        for page_number in first..=last.min(page_count) {
            selected[page_number - 1] = true;
        }
    }
    selected
        .into_iter()
        .enumerate()
        .filter_map(|(index, is_selected)| is_selected.then_some(index))
        .collect()
}

fn initialize_winrt() -> WindowsResult<()> {
    WINRT_INITIALIZED.with(|initialized| {
        if initialized.get() {
            return Ok(());
        }
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
        initialized.set(true);
        Ok(())
    })
}

pub(crate) fn shutdown() {
    WINRT_INITIALIZED.with(|initialized| {
        if initialized.replace(false) {
            unsafe { RoUninitialize() };
        }
    });
}

struct PrintSessionReservation {
    owner: isize,
}

impl Drop for PrintSessionReservation {
    fn drop(&mut self) {
        ACTIVE_PRINT_WINDOWS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.owner);
    }
}

fn begin_print_session(owner: isize) -> Option<PrintSessionReservation> {
    let mut active = ACTIVE_PRINT_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    active
        .insert(owner)
        .then(|| PrintSessionReservation { owner })
}

struct PendingPrintSession {
    manager: PrintManager,
    token: i64,
    _reservation: PrintSessionReservation,
}

struct PrintSessionCleanup {
    pending: Mutex<Option<PendingPrintSession>>,
}

impl PrintSessionCleanup {
    fn new(manager: PrintManager, token: i64, reservation: PrintSessionReservation) -> Self {
        Self {
            pending: Mutex::new(Some(PendingPrintSession {
                manager,
                token,
                _reservation: reservation,
            })),
        }
    }

    fn finish(&self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(pending) = pending {
            let _ = pending.manager.RemovePrintTaskRequested(pending.token);
        }
    }
}

pub(super) fn show_print_ui(
    owner: HWND,
    text: Arc<Vec<u16>>,
    font: FontChoice,
    display_name: &OsStr,
) -> Result<(), ModernPrintError> {
    let owner_value = owner as isize;
    let Some(reservation) = begin_print_session(owner_value) else {
        return Ok(());
    };
    initialize_winrt().map_err(classify_setup_error)?;
    match PrintManager::IsSupported() {
        Ok(true) => {}
        Ok(false) => return Err(ModernPrintError::Unavailable),
        Err(error) => return Err(classify_setup_error(error)),
    }

    let snapshot = Arc::new(DocumentSnapshot {
        text: Mutex::new(text),
        font,
        display_name: display_name.to_string_lossy().into_owned(),
        preview_dpi: unsafe { GetDpiForWindow(owner) }.max(96),
    });
    let factory: IPrintManagerInterop = unsafe {
        RoGetActivationFactory(&HSTRING::from(PRINT_MANAGER_CLASS)).map_err(classify_setup_error)?
    };
    let owner = WindowsHwnd(owner);
    let manager: PrintManager =
        unsafe { factory.GetForWindow(owner) }.map_err(classify_setup_error)?;

    let requested = TypedEventHandler::<PrintManager, PrintTaskRequestedEventArgs>::new({
        let snapshot = Arc::clone(&snapshot);
        move |_sender, args| {
            let args = args.ok()?;
            let request = args.Request()?;
            let session = Arc::new(Mutex::new(DocumentState::new(Arc::clone(&snapshot))));
            let source_session = Arc::clone(&session);
            let source_requested = PrintTaskSourceRequestedHandler::new(move |args| {
                let source: IPrintDocumentSource =
                    DocumentSource::new(Arc::clone(&source_session)).into();
                args.ok()?.SetSource(&source)
            });
            let task = request.CreatePrintTask(
                &HSTRING::from(snapshot.display_name.as_str()),
                &source_requested,
            )?;
            enable_page_ranges(&task)?;
            register_task_completion(&task, owner_value, session)?;
            Ok(())
        }
    });
    let token = manager
        .PrintTaskRequested(&requested)
        .map_err(ModernPrintError::Failed)?;
    let cleanup = Arc::new(PrintSessionCleanup::new(
        manager.clone(),
        token,
        reservation,
    ));
    let operation: IAsyncOperation<bool> = match unsafe { factory.ShowPrintUIForWindowAsync(owner) }
    {
        Ok(operation) => operation,
        Err(error) => {
            cleanup.finish();
            return Err(classify_setup_error(error));
        }
    };

    let completion_cleanup = Arc::clone(&cleanup);
    if let Err(error) = operation.when(move |result: WindowsResult<bool>| {
        completion_cleanup.finish();
        match result {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                post_async_failure(owner_value as HWND, AsyncPrintFailure::Initialization);
            }
        }
    }) {
        cleanup.finish();
        return Err(ModernPrintError::Failed(error));
    }
    Ok(())
}

fn enable_page_ranges(task: &PrintTask) -> WindowsResult<()> {
    let options = task.Options()?;
    // Page-range support was added after the base PrintManager API. Keep it a
    // best-effort enhancement so the modern path remains usable on older
    // systems that expose PrintManager but not IPrintTaskOptions2.
    let Ok(range_options) = options.PageRangeOptions() else {
        return Ok(());
    };
    range_options.SetAllowAllPages(true)?;
    range_options.SetAllowCurrentPage(false)?;
    range_options.SetAllowCustomSetOfPages(true)?;

    let Ok(custom_page_ranges) = StandardPrintTaskOptions::CustomPageRanges() else {
        return Ok(());
    };
    let displayed = options.DisplayedOptions()?;
    let mut already_displayed = false;
    for index in 0..displayed.Size()? {
        if displayed.GetAt(index)? == custom_page_ranges {
            already_displayed = true;
            break;
        }
    }
    if !already_displayed {
        displayed.Append(&custom_page_ranges)?;
    }
    Ok(())
}

fn register_task_completion(
    task: &PrintTask,
    owner: isize,
    session: Arc<Mutex<DocumentState>>,
) -> WindowsResult<()> {
    let completed =
        TypedEventHandler::<PrintTask, PrintTaskCompletedEventArgs>::new(move |_task, args| {
            let args = args.ok()?;
            let completion = args.Completion();
            release_document_session(&session);
            if completion? == PrintTaskCompletion::Failed {
                post_async_failure(owner as HWND, AsyncPrintFailure::Rendering);
            }
            Ok(())
        });
    task.Completed(&completed)?;
    Ok(())
}

fn release_document_session(session: &Mutex<DocumentState>) {
    let released = {
        let mut state = session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            state.snapshot.take(),
            state.preview_target.take(),
            state.graphics.take(),
            state.layout.take(),
        )
    };
    // Dropping COM and graphics objects may call external code. Do it only
    // after releasing the session mutex.
    drop(released);
}

fn classify_setup_error(error: Error) -> ModernPrintError {
    let code = error.code();
    if modern_print_unavailable(code) {
        ModernPrintError::Unavailable
    } else {
        ModernPrintError::Failed(error)
    }
}

fn modern_print_unavailable(code: HRESULT) -> bool {
    code == E_NOINTERFACE
        || code == E_NOTIMPL
        || code == REGDB_E_CLASSNOTREG
        || code == RO_E_METADATA_NAME_NOT_FOUND
        || code == HRESULT::from_win32(ERROR_NOT_SUPPORTED.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Foundation::{Rect, Size};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn shared_graphics_resources_are_send_and_sync() {
        assert_send_sync::<GraphicsResources>();
        assert_send_sync::<DocumentSource>();
    }

    #[test]
    fn expanded_backing_replaces_and_releases_the_original_snapshot() {
        let original = Arc::new(vec![b'a' as u16, b'\t' as u16, b'b' as u16]);
        let original_weak = Arc::downgrade(&original);
        let snapshot = DocumentSnapshot {
            text: Mutex::new(Arc::clone(&original)),
            font: FontChoice {
                logical: unsafe { std::mem::zeroed() },
                point_size_tenths: 100,
            },
            display_name: String::new(),
            preview_dpi: 96,
        };
        drop(original);

        let expanded = Arc::new("a       b".encode_utf16().collect());
        snapshot
            .make_text_authoritative(Arc::clone(&expanded))
            .unwrap();

        assert!(original_weak.upgrade().is_none());
        assert!(Arc::ptr_eq(&snapshot.text().unwrap(), &expanded));
    }

    #[test]
    fn print_session_guard_rejects_overlap_and_resets_on_drop() {
        let owner = 0x4E50_4353_isize;
        let first = begin_print_session(owner).expect("first session should start");
        assert!(begin_print_session(owner).is_none());
        drop(first);
        assert!(begin_print_session(owner).is_some());
    }

    #[test]
    fn preview_surface_uses_display_dpi_at_common_scaling_levels() {
        let desired_width = 408.0;
        assert_eq!(
            checked_surface_dimension(desired_width * 96.0 / 96.0).unwrap(),
            408
        );
        assert_eq!(
            checked_surface_dimension(desired_width * 144.0 / 96.0).unwrap(),
            612
        );
        assert_eq!(
            checked_surface_dimension(desired_width * 192.0 / 96.0).unwrap(),
            816
        );
    }

    #[test]
    fn fallback_hresult_classification_is_narrow() {
        assert!(modern_print_unavailable(E_NOINTERFACE));
        assert!(modern_print_unavailable(E_NOTIMPL));
        assert!(modern_print_unavailable(REGDB_E_CLASSNOTREG));
        assert!(modern_print_unavailable(RO_E_METADATA_NAME_NOT_FOUND));
        assert!(modern_print_unavailable(HRESULT::from_win32(
            ERROR_NOT_SUPPORTED.0
        )));
        assert!(!modern_print_unavailable(E_FAIL));
        assert!(!modern_print_unavailable(E_INVALIDARG));
    }

    #[test]
    fn page_description_uses_imageable_rect_and_quarter_inch_margin() {
        let geometry = geometry_from_page_description(PrintPageDescription {
            PageSize: Size {
                Width: 816.0,
                Height: 1056.0,
            },
            ImageableRect: Rect {
                X: 12.0,
                Y: 18.0,
                Width: 792.0,
                Height: 1020.0,
            },
            DpiX: 600,
            DpiY: 600,
        })
        .unwrap();
        assert_eq!(geometry.content_left, 36.0);
        assert_eq!(geometry.content_top, 42.0);
        assert_eq!(geometry.content_width, 744.0);
        assert_eq!(geometry.content_height, 972.0);
        assert_eq!((geometry.dpi_x, geometry.dpi_y), (600, 600));
    }

    #[test]
    fn invalid_imageable_rect_falls_back_to_full_page() {
        let geometry = geometry_from_page_description(PrintPageDescription {
            PageSize: Size {
                Width: 100.0,
                Height: 200.0,
            },
            ImageableRect: Rect::default(),
            DpiX: 0,
            DpiY: 0,
        })
        .unwrap();
        assert_eq!(geometry.content_left, 24.0);
        assert_eq!(geometry.content_top, 24.0);
        assert_eq!(geometry.content_width, 52.0);
        assert_eq!(geometry.content_height, 152.0);
        assert_eq!((geometry.dpi_x, geometry.dpi_y), (1, 1));
    }

    #[test]
    fn invalid_page_size_is_rejected() {
        assert!(
            geometry_from_page_description(PrintPageDescription {
                PageSize: Size {
                    Width: 0.0,
                    Height: 100.0,
                },
                ImageableRect: Rect::default(),
                DpiX: 96,
                DpiY: 96,
            })
            .is_none()
        );
    }

    #[test]
    fn orientation_and_media_size_changes_produce_new_layout_geometry() {
        let portrait = geometry_from_page_description(PrintPageDescription {
            PageSize: Size {
                Width: 816.0,
                Height: 1056.0,
            },
            ImageableRect: Rect {
                X: 12.0,
                Y: 18.0,
                Width: 792.0,
                Height: 1020.0,
            },
            DpiX: 600,
            DpiY: 600,
        })
        .unwrap();
        let landscape = geometry_from_page_description(PrintPageDescription {
            PageSize: Size {
                Width: 1056.0,
                Height: 816.0,
            },
            ImageableRect: Rect {
                X: 18.0,
                Y: 12.0,
                Width: 1020.0,
                Height: 792.0,
            },
            DpiX: 600,
            DpiY: 600,
        })
        .unwrap();
        let smaller_media = geometry_from_page_description(PrintPageDescription {
            PageSize: Size {
                Width: 576.0,
                Height: 864.0,
            },
            ImageableRect: Rect {
                X: 12.0,
                Y: 12.0,
                Width: 552.0,
                Height: 840.0,
            },
            DpiX: 600,
            DpiY: 600,
        })
        .unwrap();

        assert_ne!(portrait, landscape);
        assert_ne!(portrait, smaller_media);
        assert!(landscape.content_width > portrait.content_width);
        assert!(smaller_media.content_width < portrait.content_width);
    }

    #[test]
    fn page_ranges_are_clamped_sorted_and_deduplicated() {
        assert_eq!(
            normalize_page_ranges(&[(3, 5), (1, 2), (5, 20)], 7),
            vec![0, 1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn invalid_page_ranges_select_nothing() {
        assert!(normalize_page_ranges(&[(0, 0), (9, 12), (4, 2)], 5).is_empty());
    }
}
