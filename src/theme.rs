use ratatui::style::Color;

use crate::model::ThemeName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    pub(crate) canvas: Color,
    pub(crate) panel: Color,
    pub(crate) selected_surface: Color,
    pub(crate) border: Color,
    pub(crate) primary_text: Color,
    pub(crate) secondary_text: Color,
    pub(crate) muted_text: Color,
    pub(crate) accent: Color,
    pub(crate) working: Color,
    pub(crate) success: Color,
    pub(crate) info: Color,
    pub(crate) danger: Color,
}

impl From<ThemeName> for Theme {
    fn from(name: ThemeName) -> Self {
        match name {
            ThemeName::JadeDark => Self {
                canvas: Color::Rgb(11, 15, 20),
                panel: Color::Rgb(17, 24, 33),
                selected_surface: Color::Rgb(23, 49, 40),
                border: Color::Rgb(91, 102, 111),
                primary_text: Color::Rgb(232, 226, 214),
                secondary_text: Color::Rgb(154, 164, 169),
                muted_text: Color::Rgb(119, 132, 139),
                accent: Color::Rgb(104, 165, 141),
                working: Color::Rgb(213, 168, 90),
                success: Color::Rgb(131, 169, 120),
                info: Color::Rgb(113, 134, 151),
                danger: Color::Rgb(200, 120, 104),
            },
            ThemeName::MidnightDark => Self {
                canvas: Color::Rgb(13, 16, 32),
                panel: Color::Rgb(21, 26, 46),
                selected_surface: Color::Rgb(36, 40, 74),
                border: Color::Rgb(95, 101, 135),
                primary_text: Color::Rgb(233, 234, 242),
                secondary_text: Color::Rgb(167, 171, 194),
                muted_text: Color::Rgb(133, 138, 164),
                accent: Color::Rgb(139, 156, 246),
                working: Color::Rgb(231, 185, 109),
                success: Color::Rgb(120, 198, 163),
                info: Color::Rgb(125, 169, 217),
                danger: Color::Rgb(224, 122, 139),
            },
            ThemeName::PaperLight => Self {
                canvas: Color::Rgb(244, 241, 234),
                panel: Color::Rgb(255, 252, 245),
                selected_surface: Color::Rgb(221, 237, 229),
                border: Color::Rgb(145, 138, 126),
                primary_text: Color::Rgb(43, 48, 46),
                secondary_text: Color::Rgb(93, 102, 98),
                muted_text: Color::Rgb(105, 112, 108),
                accent: Color::Rgb(35, 122, 96),
                working: Color::Rgb(168, 100, 18),
                success: Color::Rgb(57, 115, 77),
                info: Color::Rgb(70, 106, 135),
                danger: Color::Rgb(169, 71, 58),
            },
            ThemeName::SandLight => Self {
                canvas: Color::Rgb(239, 231, 216),
                panel: Color::Rgb(250, 244, 232),
                selected_surface: Color::Rgb(228, 214, 184),
                border: Color::Rgb(143, 127, 105),
                primary_text: Color::Rgb(49, 44, 37),
                secondary_text: Color::Rgb(103, 95, 84),
                muted_text: Color::Rgb(108, 99, 87),
                accent: Color::Rgb(139, 94, 52),
                working: Color::Rgb(167, 93, 23),
                success: Color::Rgb(79, 116, 68),
                info: Color::Rgb(82, 111, 131),
                danger: Color::Rgb(168, 77, 63),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_keeps_ui_roles_legible() {
        for name in [
            ThemeName::JadeDark,
            ThemeName::MidnightDark,
            ThemeName::PaperLight,
            ThemeName::SandLight,
        ] {
            let theme = Theme::from(name);

            assert_ne!(theme.canvas, theme.panel, "{name:?} surfaces must differ");
            assert_ne!(
                theme.selected_surface, theme.panel,
                "{name:?} selected surface must differ"
            );
            assert_contrast(name, "border", theme.border, theme.panel, 3.0);
            assert_contrast(
                name,
                "selected text",
                theme.primary_text,
                theme.selected_surface,
                4.5,
            );

            for (role, foreground) in [
                ("primary text", theme.primary_text),
                ("secondary text", theme.secondary_text),
                ("muted text", theme.muted_text),
                ("accent", theme.accent),
                ("working", theme.working),
                ("success", theme.success),
                ("info", theme.info),
                ("danger", theme.danger),
            ] {
                assert_contrast(name, role, foreground, theme.panel, 4.5);
            }
        }
    }

    fn assert_contrast(
        theme: ThemeName,
        role: &str,
        foreground: Color,
        background: Color,
        minimum: f64,
    ) {
        let ratio = contrast_ratio(foreground, background);
        assert!(
            ratio >= minimum,
            "{theme:?} {role} contrast {ratio:.2}:1 is below {minimum:.1}:1"
        );
    }

    fn contrast_ratio(left: Color, right: Color) -> f64 {
        let left = relative_luminance(rgb(left));
        let right = relative_luminance(rgb(right));
        (left.max(right) + 0.05) / (left.min(right) + 0.05)
    }

    fn rgb(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(red, green, blue) => (red, green, blue),
            _ => panic!("themes must use explicit RGB colors"),
        }
    }

    fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
        0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }

    fn linear(channel: u8) -> f64 {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
}
