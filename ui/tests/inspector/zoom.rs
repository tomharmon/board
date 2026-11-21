use super::*;

#[test]
fn settings_zoom_reflows_the_native_window_and_survives_a_restart() {
    let f = Fixture::new();
    let size = "1050x620";
    let settings = json!({"t":0.15,"type":"tap","key":"workspace"});
    let initial = f.render(json!([settings]), size, "0.65");
    initial.expect_key("channels");
    initial.expect_key("zoom-in");
    let enlarged = f.render(
        json!([settings,
            {"t":0.4,"type":"tap","key":"zoom-in"},
            {"t":0.7,"type":"key","name":"=","cmd":true}
        ]),
        size,
        "1.1",
    );
    assert!(
        !enlarged.has_key("channels"),
        "zoom should reflow the sidebar into a drawer"
    );
    enlarged.expect_key("open-nav");
    let restarted = f.render(json!([]), size, "0.65");
    restarted.expect_key("open-nav");
    let normal = f.render(
        json!([{"t":0.25,"type":"key","name":"0","cmd":true}]),
        size,
        "0.8",
    );
    normal.expect_key("channels");
    assert_eq!(
        Board::open(&f.db).unwrap().cursor().unwrap(),
        0,
        "zoom is a window preference, not a board event"
    );
}
