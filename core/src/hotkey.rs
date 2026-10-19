//! 全局快捷键的组合键解析。只管「文本 ⇄ 组合键」，不碰任何窗口系统。
//! 界面把用户写的 "Ctrl+Alt+R" 交给这里，拿到结构化的组合键后再去抓键。

/// 主键：字母、数字、F1-F12，或少数具名键（带它在 X11 里的 keysym）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HotkeyKey {
    Char(char),
    Function(u8),
    Named(&'static str, u32),
}

/// 一个组合键。至少含一个修饰键——单独一个主键会把那个键整个抓走，不允许。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub key: HotkeyKey,
}

/// 主键名到 X11 keysym。够用就停在这，不打算覆盖整张键盘。
const NAMED_KEYS: [(&str, u32); 10] = [
    ("Space", 0x0020),
    ("Enter", 0xFF0D),
    ("Tab", 0xFF09),
    ("Esc", 0xFF1B),
    ("Home", 0xFF50),
    ("End", 0xFF57),
    ("PageUp", 0xFF55),
    ("PageDown", 0xFF56),
    ("Insert", 0xFF63),
    ("Delete", 0xFFFF),
];

impl HotkeyKey {
    /// 主键名字，用于回显到设置面板。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Char(_) | Self::Function(_) => "",
            Self::Named(name, _) => name,
        }
    }

    /// X11 keysym：字母用小写 ASCII（XK_r 是 0x72，键盘映射表里就是小写），
    /// 数字照旧，F1 起自 0xFFBE。
    pub fn keysym(self) -> u32 {
        match self {
            Self::Char(character) => u32::from(character.to_ascii_lowercase()),
            Self::Function(number) => 0xFFBD + u32::from(number),
            Self::Named(_, keysym) => keysym,
        }
    }
}

impl Hotkey {
    /// 回显用的文本，和解析格式一致：Ctrl+Alt+R。
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.logo {
            parts.push("Win".to_string());
        }
        parts.push(match self.key {
            HotkeyKey::Char(character) => character.to_string(),
            HotkeyKey::Function(number) => format!("F{number}"),
            HotkeyKey::Named(name, _) => name.to_string(),
        });
        parts.join("+")
    }
}

/// 解析 "Ctrl+Alt+R" 这样的文本。格式不对、没有修饰键或主键有多个，都返回 None。
pub fn parse_hotkey(text: &str) -> Option<Hotkey> {
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
        key: HotkeyKey::Char('R'),
    };
    let mut key: Option<HotkeyKey> = None;
    for part in text.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => hotkey.ctrl = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "super" | "meta" | "win" | "logo" => hotkey.logo = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(parse_key(other)?);
            }
        }
    }
    hotkey.key = key?;
    if !(hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.logo) {
        return None;
    }
    Some(hotkey)
}

fn parse_key(text: &str) -> Option<HotkeyKey> {
    let upper = text.to_ascii_uppercase();
    if let Some(number) = upper.strip_prefix('F') {
        if let Ok(value) = number.parse::<u8>() {
            if (1..=12).contains(&value) {
                return Some(HotkeyKey::Function(value));
            }
        }
    }
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_none() && first.is_ascii_alphanumeric() {
        return Some(HotkeyKey::Char(first.to_ascii_uppercase()));
    }
    NAMED_KEYS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(text))
        .map(|(name, keysym)| HotkeyKey::Named(name, *keysym))
}

/// 设置里用的默认值。解析不出来时退回它，保证程序总有可用的组合键。
pub fn default_main() -> Hotkey {
    parse_hotkey("Ctrl+Alt+R").expect("默认组合键必须能解析")
}

pub fn default_new() -> Hotkey {
    parse_hotkey("Ctrl+Alt+N").expect("默认组合键必须能解析")
}

/// 读设置里的文本；解析不出来就退回默认值，不让一条坏配置把快捷键废掉。
pub fn from_text(text: &str, fallback: Hotkey) -> Hotkey {
    parse_hotkey(text).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_keys() {
        let hotkey = parse_hotkey("Ctrl+Alt+R").unwrap();
        assert!(hotkey.ctrl && hotkey.alt && !hotkey.shift && !hotkey.logo);
        assert_eq!(hotkey.key, HotkeyKey::Char('R'));
        assert_eq!(hotkey.text(), "Ctrl+Alt+R");

        // 大小写与空格不敏感，回显统一成标准写法。
        assert_eq!(parse_hotkey(" ctrl + shift + f5 ").unwrap().text(), "Ctrl+Shift+F5");
        assert_eq!(parse_hotkey("Win+Space").unwrap().text(), "Win+Space");
        assert_eq!(parse_hotkey("Alt+1").unwrap().key, HotkeyKey::Char('1'));
    }

    #[test]
    fn rejects_shapes_that_would_grab_a_bare_key() {
        assert!(parse_hotkey("R").is_none(), "没有修饰键会把 R 整个抓走");
        assert!(parse_hotkey("Ctrl+").is_none());
        assert!(parse_hotkey("Ctrl+R+N").is_none(), "只能有一个主键");
        assert!(parse_hotkey("").is_none());
        assert!(parse_hotkey("Ctrl+F13").is_none(), "只支持到 F12");
        assert!(parse_hotkey("Ctrl+中").is_none());
    }

    #[test]
    fn keysyms_follow_x11_convention() {
        assert_eq!(HotkeyKey::Char('R').keysym(), 0x72);
        assert_eq!(HotkeyKey::Function(1).keysym(), 0xFFBE);
        assert_eq!(HotkeyKey::Named("Space", 0x0020).keysym(), 0x0020);
        assert_eq!(parse_hotkey("Ctrl+Enter").unwrap().key.keysym(), 0xFF0D);
    }

    #[test]
    fn broken_text_falls_back_to_default() {
        assert_eq!(from_text("", default_main()), default_main());
        assert_eq!(from_text("Ctrl+", default_new()), default_new());
        assert_eq!(from_text("Ctrl+Alt+X", default_main()).text(), "Ctrl+Alt+X");
    }
}
