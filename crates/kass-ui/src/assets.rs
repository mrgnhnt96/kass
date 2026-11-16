//! The icons Kass draws: the component library's default set plus the
//! Lucide icons Kass's screens use that it leaves out.

use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(
    KassIcons,
    [
        Box,
        Captions,
        ChartColumn,
        FileArchive,
        SlidersHorizontal,
        CircleArrowUp,
        X,
        Pencil,
        Trash,
        LayoutGrid,
        CircleQuestionMark,
        BookPlus,
        MessageSquareQuote,
        PenTool,
        NotebookPen,
        Hash,
        CircleDot,
        Mail,
        MessageCircle,
        Send,
        Lock,
        RotateCcw,
    ]
);

/// Every icon the Kass controls draw; pass to `Application::with_assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = KassIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(KassIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
