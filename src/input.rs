use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Reload,
    Back,
    Forward,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    Help,
    InputMode,
    GoToUrl,
    LinkHints,
    ScrollDown,
    ScrollUp,
    OpenExternal,
    OpenVerification,
    BrowserKey(&'static str),
    BrowserText(char),
    Click {
        column: u16,
        row: u16,
    },
    /// A left-button drag can be the first mouse event visible after a
    /// multiplexer claims a press. The app turns it into one fallback click
    /// only when no matching press was already observed.
    MousePrimaryDrag {
        column: u16,
        row: u16,
    },
    /// Likewise retain the release position as a last-resort click fallback.
    MousePrimaryUp {
        column: u16,
        row: u16,
    },
    MouseMove {
        column: u16,
        row: u16,
    },
    WheelDown,
    WheelUp,
    Escape,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    BrowserInput,
}

pub fn read_command(timeout: Duration, mode: InputMode) -> Result<Command> {
    if !event::poll(timeout)? {
        return Ok(Command::None);
    }

    match event::read()? {
        Event::Key(key) => key_command(key, mode),
        Event::Mouse(mouse) => Ok(mouse_command(mouse)),
        _ => Ok(Command::None),
    }
}

fn mouse_command(mouse: crossterm::event::MouseEvent) -> Command {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Command::Click {
            column: mouse.column,
            row: mouse.row,
        },
        MouseEventKind::Drag(MouseButton::Left) => Command::MousePrimaryDrag {
            column: mouse.column,
            row: mouse.row,
        },
        MouseEventKind::Up(MouseButton::Left) => Command::MousePrimaryUp {
            column: mouse.column,
            row: mouse.row,
        },
        MouseEventKind::Moved => Command::MouseMove {
            column: mouse.column,
            row: mouse.row,
        },
        MouseEventKind::ScrollDown => Command::WheelDown,
        MouseEventKind::ScrollUp => Command::WheelUp,
        _ => Command::None,
    }
}

fn key_command(key: KeyEvent, mode: InputMode) -> Result<Command> {
    if key.kind != KeyEventKind::Press {
        return Ok(Command::None);
    }

    if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        return Ok(Command::Quit);
    }

    // Keep a conventional browser-style tab switcher available even while a
    // page field has input focus. `{` / `}` remain useful in terminals that
    // cannot report Ctrl+Tab, but this makes the in-viewer tab model easier to
    // discover for users of desktop browsers.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        let command = match key.code {
            KeyCode::Tab => Command::NextTab,
            KeyCode::BackTab => Command::PreviousTab,
            _ => Command::None,
        };
        if command != Command::None {
            return Ok(command);
        }
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        let command = match key.code {
            KeyCode::Left => Command::Back,
            KeyCode::Right => Command::Forward,
            _ => Command::None,
        };
        return Ok(command);
    }

    if mode == InputMode::BrowserInput {
        let command = match key.code {
            KeyCode::Esc => Command::Escape,
            KeyCode::Char(c) => Command::BrowserText(c),
            KeyCode::Enter => Command::BrowserKey("Enter"),
            KeyCode::Backspace => Command::BrowserKey("Backspace"),
            KeyCode::Tab => Command::BrowserKey("Tab"),
            KeyCode::BackTab => Command::BrowserKey("Shift+Tab"),
            KeyCode::Delete => Command::BrowserKey("Delete"),
            KeyCode::Left => Command::BrowserKey("ArrowLeft"),
            KeyCode::Right => Command::BrowserKey("ArrowRight"),
            KeyCode::Up => Command::BrowserKey("ArrowUp"),
            KeyCode::Down => Command::BrowserKey("ArrowDown"),
            _ => Command::None,
        };
        return Ok(command);
    }

    let command = match key.code {
        // Esc is deliberately non-destructive in every mode. It is primarily
        // used to leave page-input mode, and a delayed mode transition must
        // never turn that intent into an unexpected application exit.
        KeyCode::Char('q') => Command::Quit,
        KeyCode::Esc => Command::Escape,
        // The toolbar uses B/F/R/V labels.  Keep the existing punctuation
        // and lower-case shortcuts, while accepting the displayed capital
        // letters too.  Lower-case `f` deliberately remains link hints.
        KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Char('[') => Command::Back,
        KeyCode::Char('F') | KeyCode::Char(']') => Command::Forward,
        KeyCode::Char('r') | KeyCode::Char('R') => Command::Reload,
        KeyCode::Char('t') => Command::NewTab,
        KeyCode::Char('x') => Command::CloseTab,
        KeyCode::Char('{') => Command::PreviousTab,
        KeyCode::Char('}') => Command::NextTab,
        KeyCode::Char('?') => Command::Help,
        KeyCode::Char('i') => Command::InputMode,
        KeyCode::Char('g') => Command::GoToUrl,
        KeyCode::Char('f') => Command::LinkHints,
        KeyCode::Char('j') | KeyCode::Down => Command::ScrollDown,
        KeyCode::Char('k') | KeyCode::Up => Command::ScrollUp,
        KeyCode::Char('o') => Command::OpenExternal,
        KeyCode::Char('v') | KeyCode::Char('V') => Command::OpenVerification,
        KeyCode::Enter => Command::BrowserKey("Enter"),
        KeyCode::Backspace => Command::BrowserKey("Backspace"),
        KeyCode::Tab => Command::BrowserKey("Tab"),
        KeyCode::BackTab => Command::BrowserKey("Shift+Tab"),
        KeyCode::Delete => Command::BrowserKey("Delete"),
        KeyCode::Left => Command::BrowserKey("ArrowLeft"),
        KeyCode::Right => Command::BrowserKey("ArrowRight"),
        _ => Command::None,
    };
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::{key_command, mouse_command, Command, InputMode};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn normal_mode_does_not_type_into_a_stale_page_focus() {
        assert_eq!(
            key_command(key(KeyCode::Char('a')), InputMode::Normal).expect("read key"),
            Command::None
        );
    }

    #[test]
    fn browser_input_mode_forwards_printable_text() {
        assert_eq!(
            key_command(key(KeyCode::Char('a')), InputMode::BrowserInput).expect("read key"),
            Command::BrowserText('a')
        );
    }

    #[test]
    fn normal_mode_keeps_tab_navigation_available() {
        assert_eq!(
            key_command(key(KeyCode::BackTab), InputMode::Normal).expect("read key"),
            Command::BrowserKey("Shift+Tab")
        );
    }

    #[test]
    fn toolbar_letter_shortcuts_match_the_visible_labels() {
        assert_eq!(
            key_command(key(KeyCode::Char('B')), InputMode::Normal).expect("read key"),
            Command::Back
        );
        assert_eq!(
            key_command(key(KeyCode::Char('F')), InputMode::Normal).expect("read key"),
            Command::Forward
        );
        assert_eq!(
            key_command(key(KeyCode::Char('R')), InputMode::Normal).expect("read key"),
            Command::Reload
        );
        assert_eq!(
            key_command(key(KeyCode::Char('V')), InputMode::Normal).expect("read key"),
            Command::OpenVerification
        );
    }

    #[test]
    fn ctrl_tab_switches_viewer_tabs_even_during_page_input() {
        let key = KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(
            key_command(key, InputMode::BrowserInput).expect("read key"),
            Command::NextTab
        );
    }

    #[test]
    fn escape_is_non_destructive_in_normal_mode() {
        assert_eq!(
            key_command(key(KeyCode::Esc), InputMode::Normal).expect("read key"),
            Command::Escape
        );
    }

    #[test]
    fn preserves_left_drag_and_release_for_nested_terminal_fallbacks() {
        let mouse = |kind| MouseEvent {
            kind,
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_command(mouse(MouseEventKind::Drag(MouseButton::Left))),
            Command::MousePrimaryDrag { column: 12, row: 7 }
        );
        assert_eq!(
            mouse_command(mouse(MouseEventKind::Up(MouseButton::Left))),
            Command::MousePrimaryUp { column: 12, row: 7 }
        );
    }
}
