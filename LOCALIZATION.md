# Localization

Notepad Classic keeps its small catalog embedded in the executable: Win32
`STRINGTABLE` and `MENU` resources are compiled with the icon into one portable
binary. Windows normally chooses the best matching resource language. If that
lookup fails, the app narrowly retries its embedded English (`en-US`) resource.
This intentionally does not use `.mui` satellites, PRI/MRT, a language selector,
or a runtime catalog parser. That trade-off keeps the standalone executable and
MSIX builds identical.

## Adding a locale

1. Copy `assets/locales/en-US.rc`, rename it for the Windows language tag, and
   set its single numeric RC `LANGUAGE` primary/sub-language pair. Choosing the
   correct Windows numeric pair for the translation is the translator's
   responsibility; the build rejects missing, malformed, or duplicate pairs.
2. Keep the main menu's popup nesting, separators, and command IDs unchanged.
   The validator supports only `IDR_MAIN_MENU MENU`, `POPUP`, `BEGIN`/`END`,
   `MENUITEM SEPARATOR`, and `MENUITEM "localized label", ID_COMMAND`; trailing
   menu flags are not supported. Translate labels, mnemonics, and shortcut
   display text only.
3. Add every `IDS_*` value from `assets/resource.h`. The build rejects missing,
   unknown, duplicate, or empty entries; zero-length localized strings are not
   supported. It also checks menu shape and numbered placeholders against
   English.
4. Add an `#include "locales/<tag>.rc"` line to `assets/notepad-classic.rc`.
   The build validates every `.rc` file in `assets/locales`; the root resource
   script decides which validated catalogs are compiled. For a shipped MSIX
   locale, also add the language to the manifest's `<Resources>` list.

`assets/resource.h` is the sole source of icon, menu, command, control, and
string IDs. Add an ID there first, then use it in every locale. `build.rs`
generates the Rust constants from that header without an extra dependency.

Use UTF-8 files, complete sentences, and numbered inserts such as `%1` rather
than translated fragments. Inserts may be reordered or repeated; `%%` is a
literal percent. In MENU labels, `&` marks a Win32 mnemonic; choose an
appropriate mnemonic and manually test for collisions. Do not add `&` to
ordinary strings or About-dialog link labels expecting mnemonic behavior. About
labels are XML-escaped before insertion into SysLink markup, while hyperlink
URLs are fixed functional constants and are never localized. Avoid localizing
file extensions/patterns, `.LOG`, class names, shell verbs, URLs, or font family
names. Paths are inserted as UTF-16 directly, so they are never lossily
converted through UTF-8. The root resource script and build invocation
explicitly select Windows code page 65001, so non-ASCII UTF-8 catalog text is
compiled consistently.

Windows-owned controls—common Open/Save, Font, Find/Replace dialogs and message
box buttons—use the language resources installed with Windows. Their language
can therefore differ from app-authored UI when a matching Windows language pack
is absent; this is expected.

Before shipping, check mnemonic collisions, clipping, Unicode rendering, and
DPI scaling. This foundation does not dynamically reflow dialogs or mirror RTL
layouts; an RTL locale needs explicit layout work and testing. MSIX/Store
metadata remains English-only: PRI/MRT and Store listing localization are out of
scope.
