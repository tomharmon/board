// Geometry from Lucide 1.49.0; see ui/licenses/Lucide.txt.
use rune::Icon;

pub(super) static PREVIOUS_FRAME: Icon = Icon {
    name: "lucide-chevron-left",
    view: 24.,
    body: r#"<path d="m15 18-6-6 6-6" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(super) static NEXT_FRAME: Icon = Icon {
    name: "lucide-chevron-right",
    view: 24.,
    body: r#"<path d="m9 18 6-6-6-6" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"/>"#,
};
pub(super) static PAUSE: Icon = Icon {
    name: "lucide-pause",
    view: 24.,
    body: r#"<g fill="currentColor"><rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/></g>"#,
};
pub(super) static SOUND: Icon = Icon {
    name: "lucide-volume-2",
    view: 24.,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5 6 9H3v6h3l5 4z"/><path d="M15 8a6 6 0 0 1 0 8m3-11a10 10 0 0 1 0 14"/></g>"#,
};
pub(super) static MUTED_SOUND: Icon = Icon {
    name: "lucide-volume-x",
    view: 24.,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M11 5 6 9H3v6h3l5 4zM16 9l6 6m0-6-6 6"/></g>"#,
};

pub(super) static LIST_FILTER: Icon = Icon {
    name: "lucide-list-filter",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M2 5h20"/><path d="M6 12h12"/><path d="M9 19h6"/></g>"#,
};
pub(super) static LIST_TODO: Icon = Icon {
    name: "lucide-list-todo",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M13 5h8"/><path d="M13 12h8"/><path d="M13 19h8"/><path d="m3 17 2 2 4-4"/><rect x="3" y="4" width="6" height="6" rx="1"/></g>"#,
};
pub(super) static MINUS: Icon = Icon {
    name: "lucide-minus",
    view: 24.,
    body: r#"<path d="M5 12h14" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round"/>"#,
};
pub(super) static PLUS: Icon = Icon {
    name: "lucide-plus",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></g>"#,
};
pub(super) static SETTINGS: Icon = Icon {
    name: "lucide-settings",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"/><circle cx="12" cy="12" r="3"/></g>"#,
};
pub(super) static CHEVRONS_UP_DOWN: Icon = Icon {
    name: "lucide-chevrons-up-down",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="m7 15 5 5 5-5"/><path d="m7 9 5-5 5 5"/></g>"#,
};
pub(super) static PANEL_LEFT_CLOSE: Icon = Icon {
    name: "lucide-panel-left-close",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18"/><path d="m16 15-3-3 3-3"/></g>"#,
};
pub(super) static PANEL_LEFT_OPEN: Icon = Icon {
    name: "lucide-panel-left-open",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><path d="M9 3v18"/><path d="m14 9 3 3-3 3"/></g>"#,
};
pub(super) static PAPERCLIP: Icon = Icon {
    name: "lucide-paperclip",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551"/></g>"#,
};
pub(super) static TAG: Icon = Icon {
    name: "lucide-tag",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z"/><circle cx="7.5" cy="7.5" r=".5" fill="currentColor"/></g>"#,
};
pub(super) static CLOUD_OFF: Icon = Icon {
    name: "lucide-cloud-off",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M10.94 5.274A7 7 0 0 1 15.71 10h1.79a4.5 4.5 0 0 1 4.222 6.057"/><path d="M18.796 18.81A4.5 4.5 0 0 1 17.5 19H9A7 7 0 0 1 5.79 5.78"/><path d="m2 2 20 20"/></g>"#,
};
pub(super) static CLOCK: Icon = Icon {
    name: "lucide-clock",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></g>"#,
};
pub(super) static AT_SIGN: Icon = Icon {
    name: "lucide-at-sign",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M16 8v5a3 3 0 0 0 6 0v-1a10 10 0 1 0-4 8"/></g>"#,
};
pub(super) static MESSAGE_SQUARE: Icon = Icon {
    name: "lucide-message-square",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M22 17a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 21.286V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2z"/></g>"#,
};
pub(super) static ELLIPSIS: Icon = Icon {
    name: "lucide-ellipsis",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></g>"#,
};
pub(super) static REPLY: Icon = Icon {
    name: "lucide-reply",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M20 18v-2a4 4 0 0 0-4-4H4"/><path d="m9 17-5-5 5-5"/></g>"#,
};
