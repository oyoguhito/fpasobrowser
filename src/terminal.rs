use crate::render::RenderSize;
use anyhow::Result;
use clap::ValueEnum;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    style::Print,
    terminal::{
        self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::io::{stdout, Stdout, Write};

/// Terminal rows reserved for fpasobrowser's own controls rather than the
/// rendered browser frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum UiChrome {
    /// Activity/status, in-viewer tab strip, and mouse toolbar.
    #[default]
    Full,
    /// Keep only the activity/status line and give two rows back to the page.
    Compact,
    /// Hide all terminal chrome while retaining one invisible image-placement
    /// guard row for nested Kitty-capable terminal clients.
    Page,
}

impl UiChrome {
    const fn reserved_rows(self) -> u16 {
        match self {
            Self::Full => 3,
            Self::Compact => 1,
            // A fully pane-height Kitty placement is anchored at the bottom
            // or clipped by the current fpasoterm/Herdr presentation path.
            // Keep the same one-row geometry as Compact, but do not draw its
            // activity text. That gives a clean page-only view without asking
            // the transport to place an image beyond its safe cell region.
            Self::Page => 1,
        }
    }

    const fn has_tab_strip(self) -> bool {
        matches!(self, Self::Full)
    }

    const fn has_toolbar(self) -> bool {
        matches!(self, Self::Full)
    }

    const fn has_activity(self) -> bool {
        !matches!(self, Self::Page)
    }
}

pub struct Terminal {
    stdout: Stdout,
    chrome: UiChrome,
}

impl Terminal {
    pub fn enter(chrome: UiChrome) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide,
            Clear(ClearType::All)
        )?;
        Ok(Self { stdout, chrome })
    }

    pub fn size(&self) -> Result<RenderSize> {
        let (columns, rows) = terminal::size()?;
        Ok(RenderSize {
            columns: columns.max(1),
            // Full chrome keeps three lines outside the browser image:
            // activity/status, an in-viewer tab strip, and a stable mouse
            // toolbar. Compact chrome keeps only the activity line.
            rows: rows.saturating_sub(self.chrome.reserved_rows()).max(1),
        })
    }

    pub const fn has_tab_strip(&self) -> bool {
        self.chrome.has_tab_strip()
    }

    pub const fn has_toolbar(&self) -> bool {
        self.chrome.has_toolbar()
    }

    pub const fn has_activity(&self) -> bool {
        self.chrome.has_activity()
    }

    pub fn writer(&mut self) -> &mut Stdout {
        &mut self.stdout
    }

    pub fn clear(&mut self) -> Result<()> {
        queue!(self.stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        Ok(())
    }

    pub fn status(&mut self, row: u16, text: &str) -> Result<()> {
        // Page-only mode deliberately has no row outside the renderer. Do not
        // issue a cursor move to `terminal_rows` here: terminals differ on
        // whether that scrolls the full-page image or silently clamps it.
        if !self.has_activity() {
            return Ok(());
        }
        queue!(
            self.stdout,
            MoveTo(0, row),
            Clear(ClearType::CurrentLine),
            Print(text)
        )?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.stdout.flush()?;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, Show, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::UiChrome;

    #[test]
    fn full_and_compact_chrome_reserve_expected_rows() {
        assert_eq!(UiChrome::Full.reserved_rows(), 3);
        assert_eq!(UiChrome::Compact.reserved_rows(), 1);
        assert_eq!(UiChrome::Page.reserved_rows(), 1);
        assert!(UiChrome::Full.has_tab_strip());
        assert!(UiChrome::Full.has_toolbar());
        assert!(!UiChrome::Compact.has_tab_strip());
        assert!(!UiChrome::Compact.has_toolbar());
        assert!(!UiChrome::Page.has_activity());
    }
}
