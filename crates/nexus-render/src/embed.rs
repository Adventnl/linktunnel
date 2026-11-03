//! Private launcher protocol. Only geometry/control travels over stdin; pixels stay native.
use crate::layout::ScaleMode;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Client area reserved by the desktop shell for the native picture.
pub struct Viewport {
    /// Left in parent client pixels.
    pub x: i32,
    /// Top in parent client pixels.
    pub y: i32,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Hide the native child beneath dialogs or inactive workspace tabs.
    pub visible: bool,
    /// Picture scaling preference.
    pub scale: ScaleMode,
}

/// Read bounded JSON lines. Oversize or malformed messages cannot grow an input allocation.
pub fn read_controls(reader: impl Read, mut notify: impl FnMut(Viewport)) {
    let mut reader = std::io::BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        let count = match reader.by_ref().take(4097).read_until(b'\n', &mut line) {
            Ok(n) => n,
            Err(_) => return,
        };
        if count == 0 || count > 4096 {
            return;
        }
        if let Ok(viewport) = serde_json::from_slice::<Viewport>(&line)
            && viewport.valid()
        {
            notify(viewport);
        }
    }
}
impl Viewport {
    /// Bound launcher coordinates to valid desktop-sized regions.
    pub fn valid(&self) -> bool {
        self.x >= 0
            && self.y >= 0
            && self.x <= 32768
            && self.y <= 32768
            && (1..=32768).contains(&self.width)
            && (1..=32768).contains(&self.height)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_validate_geometry_and_bound_lines() {
        let json = br#"{"x":2,"y":3,"width":800,"height":600,"visible":true,"scale":"fill"}
{"x":-1,"y":3,"width":800,"height":600,"visible":true,"scale":"fit"}
invalid
"#;
        let mut got = Vec::new();
        read_controls(json.as_slice(), |v| got.push(v));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].scale, ScaleMode::Fill);
        read_controls(vec![b'x'; 5000].as_slice(), |_| panic!("oversized input"));
    }
}
