//! Local reaction names are stored independently of a client's choice of glyph.
use anyhow::{Result, ensure};

// Glyphs and alternate public spellings of the same board reaction. The canonical
// names themselves always take precedence over an alias (e.g. "cry").
const SPELLINGS: &str = "
100 💯
angry 😠
arrow_down ⬇
arrow_left ⬅
arrow_right ➡
arrow_up ⬆
balloon 🎈
beer 🍺
bell 🔔
black_circle ⚫
blue_circle 🔵 large_blue_circle
boom 💥
bug 🐛
calendar 📅
chart_down 📉 chart_with_downwards_trend
chart_up 📈 chart_with_upwards_trend
check ✅ ✔ white_check_mark heavy_check_mark
clap 👏
clipboard 📋
clock 🕐 clock1
cloud ☁
coffee ☕
confetti 🎊 confetti_ball
confused 😕
cool 😎 sunglasses
cry 😭 sob
email 📧
exclamation ❗
eyes 👀
facepalm 🤦
file 📄 page_facing_up
fire 🔥
folder 📁 file_folder
gear ⚙
gift 🎁
green_circle 🟢 large_green_circle
grimace 😬 grimacing
hammer 🔨
heart ❤
hourglass ⏳
hug 🤗 hugging_face
inbox 📥 inbox_tray
info ℹ information_source
key 🔑
laugh 😂 😆 laughing satisfied joy
lightbulb 💡 bulb
lightning ⚡ zap
link 🔗
lock 🔒
love_eyes 😍 heart_eyes
medal 🥇 first_place_medal
megaphone 📢 mega
memo 📝
mind_blown 🤯 exploding_head
muscle 💪
neutral 😐 neutral_face
ok_hand 👌
outbox 📤 outbox_tray
package 📦
party 🎉 🥳 tada partying_face
pin 📌 pushpin
pizza 🍕
point_down 👇
point_left 👈
point_right 👉
point_up 👆
pray 🙏
question ❓ ?
rain 🌧 rain_cloud
rainbow 🌈
raised_hands 🙌
red_circle 🔴
refresh 🔄 arrows_counterclockwise
relieved 😌
rocket 🚀
rolling_eyes 🙄
rotating_light 🚨
sad 😢 white_frowning_face
ship 🚢
shrug 🤷
sick 🤢 nauseated_face
sleeping 😴
smile 😊 slightly_smiling_face
snow ❄ snowflake
sparkles ✨
speech_bubble 💬 speech_balloon
star ⭐
stop 🛑 octagonal_sign
sun ☀ sunny
surprised 😮 open_mouth
thinking 🤔 thinking_face
thumbs_down 👎 -1 thumbsdown
thumbs_up 👍 +1 thumbsup
trophy 🏆
unlock 🔓
warning ⚠
wave 👋
white_circle ⚪
wink 😉
worried 😟
wrench 🔧
x ❌ ✖ heavy_multiplication_x
yellow_circle 🟡 large_yellow_circle
zany 🤪 zany_face
";

fn name_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'+' | b'-')
}

/// A commonly used platform shortcode (e.g. `+1`); custom names are unchanged.
pub fn shortcode(name: &str) -> &str {
    SPELLINGS
        .lines()
        .find_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != name {
                return None;
            }
            words.find(|word| word.bytes().all(name_byte))
        })
        .unwrap_or(name)
}

pub fn normalize(input: &str) -> Result<String> {
    let lower = input.trim().to_ascii_lowercase();
    let name = lower.strip_prefix(':').unwrap_or(&lower);
    let name = name.strip_suffix(':').unwrap_or(name);
    let named = !name.is_empty() && name.bytes().all(name_byte);
    let bare = lower.replace('\u{fe0f}', "");
    let spelling = if named { name } else { &bare };
    let mapped = SPELLINGS.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let canonical = words.next()?;
        (canonical == spelling || words.any(|s| s == spelling)).then_some(canonical)
    });
    let name = mapped.unwrap_or(name);
    ensure!(
        name.len() <= 64
            && (mapped.is_some() || named)
            && !matches!(name, "__proto__" | "prototype" | "constructor"),
        "invalid reaction"
    );
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_known_reactions_roundtrip_through_their_public_shortcodes() {
        for line in SPELLINGS.lines() {
            let mut words = line.split_whitespace();
            let Some(canonical) = words.next() else {
                continue;
            };
            for form in words {
                assert_eq!(normalize(form).unwrap(), canonical);
            }
            assert_eq!(normalize(shortcode(canonical)).unwrap(), canonical);
        }
        assert_eq!(normalize(" :BESPOKE: ").unwrap(), "bespoke");
        assert_eq!(normalize("⚙️").unwrap(), "gear");
        for denied in [
            "",
            "__proto__",
            "prototype",
            "constructor",
            "::eyes::",
            "a/b",
            "??",
        ] {
            assert!(normalize(denied).is_err(), "{denied}");
        }
    }
}
