//! Headless UI tests: real key events into the real `.slint` component.
use super::rows::{IconCache, to_item};
use super::*;
use crate::plugin;
use slint::platform::{Key, WindowEvent};
use std::cell::RefCell;
use std::rc::Rc;

fn press(ui: &MainWindow, key: Key) {
    let text: SharedString = char::from(key).to_string().into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

/// Ctrl+C, as a desktop delivers it: `c` while Control is held.
fn ctrl_c(ui: &MainWindow) {
    let ctrl: SharedString = char::from(Key::Control).to_string().into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: ctrl.clone() });
    press_text(ui, "c");
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: ctrl });
}

fn press_text(ui: &MainWindow, t: &str) {
    let text: SharedString = t.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn type_text(ui: &MainWindow, s: &str) {
    for c in s.chars() {
        let text: SharedString = c.to_string().into();
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text });
    }
}

fn setup() -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    let ui = MainWindow::new().unwrap();
    let item = |t: &str| Item {
        title: t.into(),
        subtitle: "".into(),
        glyph: "x".into(),
        icon: slint::Image::default(),
        has_icon: false,
        fresh: false,
    };
    ui.set_items(ModelRc::new(VecModel::from(vec![item("one"), item("two")])));
    let action = |t: &str, d| ActionItem { title: t.into(), is_default: d };
    ui.set_actions(ModelRc::new(VecModel::from(vec![action("Terminate", true), action("Force kill", false)])));
    ui.show().unwrap();
    ui.invoke_focus_input();
    ui
}

#[test]
fn tab_requests_the_menu_for_the_selected_result_and_menu_keys_work() {
    let ui = setup();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = log.clone();
    ui.on_open_menu(move |i| l.borrow_mut().push(format!("open {i}")));
    let l = log.clone();
    ui.on_run_action(move |r, a| l.borrow_mut().push(format!("run {r} {a}")));
    let l = log.clone();
    ui.on_dismiss(move || l.borrow_mut().push("dismiss".into()));

    press(&ui, Key::DownArrow);
    press(&ui, Key::Tab);
    assert_eq!(*log.borrow(), ["open 1"], "Tab asks Rust to open the menu for result 1");

    // Rust would now fill the actions and open the menu.
    ui.set_menu_open(true);
    press(&ui, Key::DownArrow);
    assert_eq!(ui.get_action_selected(), 1);
    press(&ui, Key::DownArrow);
    assert_eq!(ui.get_action_selected(), 1, "selection clamps at the last action");
    press(&ui, Key::UpArrow);
    press(&ui, Key::UpArrow);
    assert_eq!(ui.get_action_selected(), 0);
    press(&ui, Key::DownArrow);
    press(&ui, Key::Return);
    assert_eq!(log.borrow().last().unwrap(), "run 1 1", "Enter runs the highlighted action of the selected result");

    type_text(&ui, "abc");
    assert_eq!(ui.get_query(), "", "typing is ignored while the menu is open");

    ctrl_c(&ui);
    assert!(!ui.get_menu_open(), "Ctrl+C closes the menu");
    assert!(!log.borrow().contains(&"dismiss".to_string()), "...without hiding the launcher");
    ui.set_menu_open(true);
    press(&ui, Key::Escape);
    assert_eq!(log.borrow().last().unwrap(), "dismiss", "Esc hides the launcher, even from the menu");
}

#[test]
fn tab_closes_the_menu_and_editing_the_query_does_too() {
    let ui = setup();
    ui.set_menu_open(true);
    press(&ui, Key::Tab);
    assert!(!ui.get_menu_open());

    ui.set_menu_open(true);
    press(&ui, Key::LeftArrow);
    assert!(!ui.get_menu_open());
}

#[test]
fn complete_search_fills_the_box_and_triggers_a_search() {
    let ui = setup();
    let searched: Rc<RefCell<Vec<String>>> = Rc::default();
    let s = searched.clone();
    ui.on_query_changed(move |t| s.borrow_mut().push(t.to_string()));
    type_text(&ui, "ki");
    super::complete_search(&ui, "kill ");
    assert_eq!(ui.get_query(), "kill ");
    assert_eq!(searched.borrow().last().unwrap(), "kill ", "the plugin's results must load, not stop at a menu");
}

#[test]
fn tab_does_nothing_without_results() {
    let ui = setup();
    ui.set_items(ModelRc::default());
    let opened = Rc::new(RefCell::new(false));
    let o = opened.clone();
    ui.on_open_menu(move |_| *o.borrow_mut() = true);
    press(&ui, Key::Tab);
    assert!(!*opened.borrow());
}

#[test]
fn complete_sets_the_text_with_the_cursor_at_the_end() {
    let ui = setup();
    type_text(&ui, "ki");
    ui.invoke_complete("kill ".into());
    assert_eq!(ui.get_query(), "kill ");
    type_text(&ui, "fire");
    assert_eq!(ui.get_query(), "kill fire", "typing continues after the completed command");
}

#[test]
#[allow(clippy::float_cmp, reason = "layout heights are exact sums of constants")]
fn window_height_follows_the_mode() {
    let ui = setup();
    // input 68 + divider 1 + list padding 2*8 + rows * 52
    assert_eq!(ui.get_wanted_height(), 68.0 + 1.0 + 16.0 + 2.0 * 52.0, "input + 2 results");
    ui.set_menu_open(true);
    assert_eq!(ui.get_wanted_height(), 68.0 + 1.0 + 16.0 + 3.0 * 52.0, "input + header + 2 actions");
    ui.set_busy(true);
    assert_eq!(ui.get_wanted_height(), 68.0, "a running action hides the menu");
    ui.set_menu_open(false);
    assert_eq!(ui.get_wanted_height(), 68.0, "and the results");
    ui.set_busy(false);
    ui.set_items(ModelRc::default());
    assert_eq!(ui.get_wanted_height(), 68.0, "input only: no divider, no padding");
}

#[test]
fn output_panel_keys_go_to_the_panel() {
    let ui = setup();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = log.clone();
    ui.on_output_action(move |i| l.borrow_mut().push(format!("action {i}")));
    let l = log.clone();
    ui.on_output_menu(move || l.borrow_mut().push("menu".into()));
    let l = log.clone();
    ui.on_output_back(move || l.borrow_mut().push("back".into()));
    let l = log.clone();
    ui.on_dismiss(move || l.borrow_mut().push("dismiss".into()));
    let l = log.clone();
    ui.on_run_action(move |r, a| l.borrow_mut().push(format!("run {r} {a}")));

    type_text(&ui, "ai x");
    ui.set_output_open(true);
    let l = log.clone();
    ui.on_query_changed(move |t| l.borrow_mut().push(format!("search {t}")));
    press(&ui, Key::Backspace);
    assert_eq!(ui.get_query(), "ai ", "an edit reaches the search box");
    assert_eq!(*log.borrow(), ["back", "search ai "], "and leaves the panel for the results");
    log.borrow_mut().clear();
    ui.set_output_open(true);
    type_text(&ui, "y");
    assert_eq!(ui.get_query(), "ai y");
    assert_eq!(*log.borrow(), ["back", "search ai y"], "typing too");
    log.borrow_mut().clear();
    ui.set_output_open(true);
    press(&ui, Key::LeftArrow);
    assert!(log.borrow().is_empty(), "moving the cursor is not an edit: {:?}", log.borrow());
    press(&ui, Key::Return);
    press(&ui, Key::Tab);
    assert_eq!(*log.borrow(), ["action -1", "menu"], "Enter runs the default, Tab asks for the menu");

    // Rust opens the menu with the panel's actions; Enter runs the chosen one as an output action.
    ui.set_menu_open(true);
    press(&ui, Key::DownArrow);
    press(&ui, Key::Return);
    assert_eq!(log.borrow().last().unwrap(), "action 1");
    ctrl_c(&ui);
    assert!(!ui.get_menu_open(), "Ctrl+C closes the menu first");
    assert_eq!(log.borrow().last().unwrap(), "action 1");

    ui.set_output_status(1); // done
    ctrl_c(&ui);
    assert_eq!(log.borrow().last().unwrap(), "back", "then goes back to the results");
    assert!(!log.borrow().iter().any(|e| e == "dismiss" || e.starts_with("run")), "{:?}", log.borrow());

    press(&ui, Key::Escape);
    assert_eq!(log.borrow().last().unwrap(), "dismiss", "Esc hides the launcher from the panel too");
}

#[test]
fn ctrl_c_steps_back_without_closing() {
    let ui = setup();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    for (name, cb) in [("stop", 0), ("back", 1), ("dismiss", 2)] {
        let l = log.clone();
        let f = move || l.borrow_mut().push(name.to_string());
        match cb {
            0 => ui.on_output_stop(f),
            1 => ui.on_output_back(f),
            _ => ui.on_dismiss(f),
        }
    }
    let l = log.clone();
    ui.on_query_changed(move |t| l.borrow_mut().push(format!("search {t:?}")));

    type_text(&ui, "fire");
    ctrl_c(&ui);
    assert_eq!(ui.get_query(), "", "clears the search");
    assert_eq!(log.borrow().last().unwrap(), "search \"\"", "and shows the empty list");
    ctrl_c(&ui); // nothing left to clear: no-op

    // Answer streaming, follow-up typed: stop first, then clear, then back.
    ui.set_output_open(true);
    ui.set_output_input(true);
    ui.set_output_status(0);
    type_text(&ui, "why");
    ctrl_c(&ui);
    assert_eq!(log.borrow().last().unwrap(), "stop");
    assert_eq!(ui.get_query(), "why", "the follow-up is kept while stopping");
    ui.set_output_status(2); // Rust marks it stopped
    ctrl_c(&ui);
    assert_eq!(ui.get_query(), "");
    ctrl_c(&ui);
    assert_eq!(log.borrow().last().unwrap(), "back");
    assert!(!log.borrow().iter().any(|e| e == "dismiss"), "never closes: {:?}", log.borrow());

    // With text selected, Ctrl+C copies instead (the key reaches the text field).
    ui.set_output_open(false);
    type_text(&ui, "abc");
    ui.invoke_select_all();
    ctrl_c(&ui);
    assert_eq!(ui.get_query(), "abc", "a selection is copied, not cleared");
}

#[test]
fn output_panel_follow_up_input() {
    let ui = setup();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = log.clone();
    ui.on_output_send(move |t| l.borrow_mut().push(format!("send {t}")));
    let l = log.clone();
    ui.on_output_action(move |i| l.borrow_mut().push(format!("action {i}")));
    let l = log.clone();
    ui.on_query_changed(move |t| l.borrow_mut().push(format!("search {t}")));
    let l = log.clone();
    ui.on_output_back(move || l.borrow_mut().push("back".into()));

    ui.set_output_open(true);
    ui.set_output_input(true);
    press(&ui, Key::Return);
    assert_eq!(log.borrow().last().unwrap(), "action -1", "empty input: Enter runs the default (Copy)");
    type_text(&ui, "why?");
    assert_eq!(ui.get_query(), "why?", "typing goes to the follow-up input");
    assert!(!log.borrow().iter().any(|e| e.starts_with("search")), "and does not search: {:?}", log.borrow());
    press(&ui, Key::LeftArrow);
    assert_ne!(log.borrow().last().unwrap(), "back", "Left moves the cursor while there is text");
    press(&ui, Key::Return);
    assert_eq!(log.borrow().last().unwrap(), "send why?");
}

#[test]
#[allow(clippy::float_cmp, reason = "layout heights are exact sums of constants")]
fn output_panel_height_follows_its_text() {
    let ui = setup();
    let list = ui.get_wanted_height();
    ui.set_output_open(true);
    let short = ui.get_wanted_height();
    // input 68 + divider + three lines (60) + padding 2*14 + divider + footer 34
    assert_eq!(short, 68.0 + 1.0 + 60.0 + 28.0 + 1.0 + 34.0, "an empty panel still shows three lines");
    ui.set_output_text("line\n".repeat(200).into());
    let tall = ui.get_wanted_height();
    assert!(tall > short, "grows with the text");
    assert_eq!(tall, list.max(68.0 + 1.0 + 16.0 + 8.0 * 52.0), "but never past the height of 8 rows");
    ui.set_menu_open(true);
    assert_eq!(ui.get_wanted_height(), 68.0 + 1.0 + 16.0 + 3.0 * 52.0, "its actions menu replaces it");
    ui.set_menu_open(false);
    ui.set_output_open(false);
    assert_eq!(ui.get_wanted_height(), list, "closing brings the results back");
}

#[test]
fn output_panel_follows_new_text_until_scrolled_up() {
    let ui = setup();
    let settle = || slint::platform::update_timers_and_animations();
    ui.set_output_open(true);
    ui.set_output_text("line\n".repeat(200).into());
    settle();
    assert!(ui.get_output_follow(), "a growing answer stays at its end");
    press(&ui, Key::UpArrow);
    settle();
    assert!(!ui.get_output_follow(), "scrolling up stops following");
    ui.set_output_text("line\n".repeat(220).into());
    settle();
    assert!(!ui.get_output_follow(), "more text does not pull the view back down");
    press(&ui, Key::PageDown);
    press(&ui, Key::PageDown);
    settle();
    assert!(ui.get_output_follow(), "reaching the end follows again");
    press(&ui, Key::UpArrow);
    settle();
    ui.invoke_follow_output();
    settle();
    assert!(ui.get_output_follow(), "a new answer starts at the end");
}

#[test]
fn theme_follows_the_system_unless_forced() {
    let ui = setup();
    let theme = ui.global::<Theme>();
    theme.set_system_dark(true);
    theme.set_mode(0);
    assert!(theme.get_dark());
    theme.set_system_dark(false);
    assert!(!theme.get_dark(), "auto follows the system setting");
    let light_bg = theme.get_bg();
    theme.set_mode(1);
    assert!(theme.get_dark(), "forced dark ignores the system");
    assert_ne!(theme.get_bg(), light_bg, "the palette really changes");
    theme.set_mode(2);
    theme.set_system_dark(true);
    assert!(!theme.get_dark(), "forced light ignores the system");
    assert_eq!(theme.get_bg(), light_bg);
}

#[test]
fn rows_show_an_image_only_when_one_loaded() {
    let r = |icon: Option<plugin::Icon>| ResultItem {
        id: "1".into(),
        title: "firefox".into(),
        subtitle: String::new(),
        icon,
        score: 1.0,
        actions: vec![],
        complete: None,
        fallback: false,
        provider: String::new(),
        query: String::new(),
    };
    let _ui = setup(); // installs the testing backend, needed to decode images
    let tmp = crate::test_support::tmp();
    let png = crate::test_support::png_32(tmp.path());
    let with = to_item(
        &r(Some(plugin::Icon { path: Some(png.to_string_lossy().into()), ..Default::default() })),
        &mut IconCache::default(),
    );
    assert!(with.has_icon);
    assert_eq!(with.icon.size().width, 32);
    let missing = to_item(
        &r(Some(plugin::Icon { path: Some("/no/such.png".into()), ..Default::default() })),
        &mut IconCache::default(),
    );
    assert!(!missing.has_icon, "unloadable file falls back to the letter tile");
    assert_eq!(missing.glyph, "F");
    let glyph =
        to_item(&r(Some(plugin::Icon { glyph: Some("=".into()), ..Default::default() })), &mut IconCache::default());
    assert_eq!((glyph.has_icon, glyph.glyph.as_str()), (false, "="));
}

#[test]
fn only_the_margin_grip_and_header_start_a_drag_the_whole_bar_is_the_text_field() {
    use slint::LogicalPosition;
    let ui = setup();
    ui.window().set_size(slint::LogicalSize::new(640.0, 200.0));
    let dragged = Rc::new(RefCell::new(0));
    let d = dragged.clone();
    ui.on_start_drag(move || *d.borrow_mut() += 1);
    let click = |x: f32, y: f32| {
        let position = LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    };
    click(10.0, 34.0); // left margin
    click(620.0, 34.0); // right-hand grip
    assert_eq!(*dragged.borrow(), 2, "margin and grip drag");
    // Everywhere else in the bar is the text field, including its top and bottom edges.
    for (x, y) in [(300.0, 34.0), (300.0, 3.0), (300.0, 64.0), (100.0, 10.0), (560.0, 60.0)] {
        click(x, y);
    }
    assert_eq!(*dragged.borrow(), 2, "clicks in the bar must place the caret, not drag");
    click(300.0, 120.0); // a result row: activates, not a drag
    assert_eq!(*dragged.borrow(), 2);
}

#[test]
fn empty_launcher_text_field_clicks_do_not_drag() {
    use slint::LogicalPosition;
    let ui = setup();
    ui.set_items(ModelRc::default()); // nothing typed, no results: the window is only 68 px tall
    ui.window().set_size(slint::LogicalSize::new(640.0, ui.get_wanted_height()));
    let dragged = Rc::new(RefCell::new(Vec::<(f32, f32)>::new()));
    let mut hits = vec![];
    for (x, y) in [
        (300.0, 34.0),
        (100.0, 34.0),
        (580.0, 20.0),
        (400.0, 30.0),
        (240.0, 38.0),
        (90.0, 35.0),
        (490.0, 39.0),
        (560.0, 2.0),
        (300.0, 66.0),
        (40.0, 67.0),
    ] {
        let d = dragged.clone();
        ui.on_start_drag(move || d.borrow_mut().push((x, y)));
        let position = LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        hits.push((x, y, dragged.borrow().len()));
    }
    eprintln!("drag callbacks after each click: {hits:?}");
    assert!(dragged.borrow().is_empty(), "clicks inside the text field must not start a drag: {:?}", dragged.borrow());
}

/// Dev tool, not a check: renders the window offscreen to PPM files in $KAIROS_SHOTS.
/// `KAIROS_SHOTS=/some/dir cargo test render_screenshots -- --ignored`
#[test]
#[ignore = "dev tool that writes screenshots, not a check"]
fn render_screenshots() {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter};

    struct P(Rc<MinimalSoftwareWindow>);
    impl Platform for P {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    let out = std::path::PathBuf::from(std::env::var("KAIROS_SHOTS").expect("set KAIROS_SHOTS"));
    std::fs::create_dir_all(&out).unwrap();
    let win = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(P(win.clone()))).unwrap();

    let ui = MainWindow::new().unwrap();
    let tmp = crate::test_support::tmp();
    let demo = crate::test_support::png_32(tmp.path());
    let icon = |p: &std::path::Path| plugin::Icon { path: Some(p.to_string_lossy().into()), ..Default::default() };
    let r = |title: &str, sub: &str, icon: Option<plugin::Icon>| ResultItem {
        id: title.into(),
        title: title.into(),
        subtitle: sub.into(),
        icon,
        score: 1.0,
        actions: vec![],
        complete: None,
        fallback: false,
        provider: String::new(),
        query: String::new(),
    };
    let rows = [
        r("Firefox", "Web Browser", Some(icon(&demo))),
        r("Files", "Access and organize files", Some(icon(&demo))),
        r("Calculator", "Perform arithmetic and scientific calculations", None),
        r(
            "kill",
            "Process Killer: Find and terminate your own running processes",
            Some(plugin::Icon { glyph: Some("›".into()), ..Default::default() }),
        ),
        r("Visual Studio Code", "", None),
        r("grinning face", "Emoji · :grinning:", Some(plugin::Icon { glyph: Some("😀".into()), ..Default::default() })),
    ];
    let items: Vec<Item> = rows.iter().map(|r| to_item(r, &mut IconCache::default())).collect();
    ui.set_items(ModelRc::new(VecModel::from(items)));
    ui.set_actions(ModelRc::new(VecModel::from(vec![
        ActionItem { title: "Terminate (graceful)".into(), is_default: true },
        ActionItem { title: "Force kill".into(), is_default: false },
    ])));
    ui.show().unwrap();

    let shot = |name: &str, dark: bool, query: &str, menu: bool, selected: i32| {
        let theme = ui.global::<Theme>();
        theme.set_mode(if dark { 1 } else { 2 });
        ui.set_query(query.into());
        ui.set_menu_open(menu);
        ui.set_selected(selected);
        let h = ui.get_wanted_height().ceil() as u32;
        win.set_size(slint::PhysicalSize::new(640, h));
        let mut buf = vec![PremultipliedRgbaColor::default(); (640 * h) as usize];
        win.draw_if_needed(|renderer| {
            renderer.render(&mut buf, 640);
        });
        // Composite over mid-grey so the transparent rounded corners are visible.
        let mut ppm = format!("P6\n640 {h}\n255\n").into_bytes();
        for p in &buf {
            let bg = 128u16;
            let inv = 255 - u16::from(p.alpha);
            ppm.extend_from_slice(&[
                (u16::from(p.red) + bg * inv / 255) as u8,
                (u16::from(p.green) + bg * inv / 255) as u8,
                (u16::from(p.blue) + bg * inv / 255) as u8,
            ]);
        }
        std::fs::write(out.join(format!("{name}.ppm")), ppm).unwrap();
    };
    shot("dark-results", true, "fi", false, 0);
    shot("light-results", false, "fi", false, 1);
    shot("dark-empty", true, "", false, 0);
    shot("dark-menu", true, "kill fire", true, 3);
    shot("light-menu", false, "kill fire", true, 3);
    ui.set_busy(true);
    shot("dark-busy", true, "kill fire", false, 0);
    shot("light-busy", false, "kill fire", false, 0);
    ui.set_busy(false);
    ui.set_output_open(true);
    ui.set_output_title("Claude · Ask AI".into());
    ui.set_output_text(
        "Lifetimes tell the compiler how long a reference stays valid. Every reference has one; \
         most are inferred, so you only write them when a function returns a reference tied to \
         one of its inputs."
            .into(),
    );
    shot("dark-output", true, "ai explain rust lifetimes", false, 0);
    shot("light-output", false, "ai explain rust lifetimes", false, 0);
    ui.set_output_text("Paragraph after paragraph.\n\n".repeat(40).into());
    ui.set_output_status(1);
    shot("dark-output-long", true, "ai explain rust lifetimes", false, 0);
    ui.set_output_status(2);
    ui.set_output_message("Stopped: no answer from the plugin".into());
    shot("light-output-error", false, "ai explain rust lifetimes", false, 0);
    ui.set_output_status(1);
    ui.set_output_markdown(true);
    ui.set_output_styled(super::markdown::to_styled(
        "## Lifetimes\n\nA **lifetime** names how long a *reference* stays valid. See [the book](https://doc.rust-lang.org/book/).\n\n\
         1. Most are inferred\n2. Write them when a function returns a reference:\n   - tied to an input\n   - or `'static`\n\n\
         ```rust\nfn longest<'a>(x: &'a str, y: &'a str) -> &'a str {\n    if x.len() > y.len() { x } else { y }\n}\n```\n\n\
         > Borrowed values must outlive their uses.\n\n| Kind | Example |\n|---|---|\n| shared | `&T` |\n| unique | `&mut T` |",
    ));
    shot("dark-output-markdown", true, "ai explain rust lifetimes", false, 0);
    shot("light-output-markdown", false, "ai explain rust lifetimes", false, 0);
    ui.set_output_open(false);

    let mut rows = super::settings::demo_rows(
        include_str!("../../plugins/ai/plugin.toml"),
        &serde_json::json!({"api": "anthropic", "base_url": "https://api.anthropic.com", "model": "claude-opus-5-5",
            "api_key": "x", "max_tokens": 0, "secrets": {"github_token": "y"},
            "mcp_servers": {"files": {"command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home/me/notes"]}}}),
    );
    rows[2].text = "claude-sonnet-5-5".into();
    rows[2].dirty = true;
    ui.set_settings_title("AI settings".into());
    ui.set_settings_fields(ModelRc::new(VecModel::from(rows.clone())));
    ui.set_settings_dirty(1);
    ui.set_settings_open(true);
    shot("dark-settings", true, "plugin ai", false, 0);
    shot("light-settings", false, "plugin ai", false, 0);
    rows[6].error = "expected a number".into();
    ui.set_settings_fields(ModelRc::new(VecModel::from(rows[6..].to_vec())));
    ui.set_settings_status("Not saved: see the marked fields".into());
    ui.set_settings_status_error(true);
    shot("dark-settings-error", true, "plugin ai", false, 0);
    let ai_values = serde_json::json!({"mcp_servers": {
        "files": {"command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home/me/notes"],
                  "env": {"LOG_LEVEL": "warn"}, "allow": ["read_text_file", "list_directory"]},
        "github": {"url": "https://api.githubcopilot.com/mcp/", "headers": {"Authorization": "Bearer ${github_token}"}}}});
    let rows = super::settings::demo_rows(include_str!("../../plugins/ai/plugin.toml"), &ai_values);
    ui.set_settings_fields(ModelRc::new(VecModel::from(rows[7..].to_vec())));
    ui.set_settings_status("".into());
    ui.set_settings_status_error(false);
    shot("dark-settings-entries", true, "plugin ai", false, 0);
    let entry = super::settings::demo_entry_rows(
        include_str!("../../plugins/ai/plugin.toml"),
        &ai_values,
        "mcp_servers",
        "files",
    );
    ui.set_settings_fields(ModelRc::new(VecModel::from(entry.clone())));
    ui.set_settings_title("AI settings › files".into());
    shot("light-settings-entry", false, "plugin ai", false, 0);
    ui.set_settings_fields(ModelRc::new(VecModel::from(entry[2..].to_vec())));
    shot("dark-settings-entry-pairs", true, "plugin ai", false, 0);
    let actions = include_str!("../builtin/actions/plugin.toml");
    let action_values = serde_json::json!({"actions": {
        "gh": {"title": "GitHub repository", "command": "xdg-open https://github.com/{}"},
        "logs": {"command": "journalctl -f -u {1}", "terminal": true, "default": "nginx"}}});
    ui.set_settings_title("Custom actions settings".into());
    ui.set_settings_fields(ModelRc::new(VecModel::from(super::settings::demo_rows(actions, &action_values))));
    shot("dark-settings-actions", true, "plugin builtin-actions", false, 0);
    let entry = super::settings::demo_entry_rows(actions, &action_values, "actions", "logs");
    ui.set_settings_fields(ModelRc::new(VecModel::from(entry)));
    ui.set_settings_title("Custom actions settings › logs".into());
    shot("light-settings-action", false, "plugin builtin-actions", false, 0);
    ui.set_settings_title("AI settings".into());
    ui.set_settings_fields(ModelRc::new(VecModel::from(super::settings::demo_rows(
        include_str!("../builtin/files/plugin.toml"),
        &serde_json::json!({"roots": ["~", "/data/projects"], "hidden": true, "max_depth": 8}),
    ))));
    ui.set_settings_title("Files settings".into());
    ui.set_settings_status("".into());
    ui.set_settings_status_error(false);
    ui.set_settings_dirty(0);
    shot("light-settings-files", false, "plugin files", false, 0);
}

#[test]
fn select_all_lets_typing_replace_the_restored_query() {
    let ui = setup();
    ui.set_query("old search".into());
    ui.invoke_select_all();
    type_text(&ui, "x");
    assert_eq!(ui.get_query(), "x");
}

#[test]
fn a_search_is_kept_only_for_a_while() {
    let t = Instant::now();
    let keep = Duration::from_secs(90);
    assert!(resumable(t, t + Duration::from_secs(10), keep));
    assert!(resumable(t, t + keep, keep), "at the limit");
    assert!(!resumable(t, t + keep + Duration::from_millis(1), keep));
    assert!(!resumable(t, t, Duration::ZERO), "0 turns it off");
}

#[test]
fn settings_page_takes_the_keys_tab_moves_between_fields() {
    let ui = setup();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l = log.clone();
    ui.on_settings_edited(move |i, t| l.borrow_mut().push(format!("edit {i} {t}")));
    let l = log.clone();
    ui.on_settings_save(move || l.borrow_mut().push("save".into()));
    let l = log.clone();
    ui.on_settings_back(move || l.borrow_mut().push("back".into()));
    let l = log.clone();
    ui.on_dismiss(move || l.borrow_mut().push("dismiss".into()));
    let rows = super::settings::demo_rows(
        "id = \"p\"\nname = \"P\"\nversion = \"1\"\n\
         [[settings]]\nkey = \"a\"\ntype = \"string\"\n\
         [[settings]]\nkey = \"b\"\ntype = \"bool\"\ndefault = false\n\
         [[settings]]\nkey = \"c\"\ntype = \"string\"\n",
        &serde_json::json!({}),
    );
    ui.set_settings_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_settings_autofocus(true);
    ui.set_settings_open(true);
    // Rows are created lazily, by the next frame in the app; any walk of the tree does it here.
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(5.0, 5.0) });
    assert!(ui.get_wanted_height() > 68.0 + 34.0 + 3.0 * 40.0, "room for the rows: {}", ui.get_wanted_height());
    type_text(&ui, "x");
    press(&ui, Key::Tab);
    press_text(&ui, " ");
    press(&ui, Key::Tab);
    type_text(&ui, "y");
    let ctrl: SharedString = char::from(Key::Control).to_string().into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: ctrl.clone() });
    press_text(&ui, "s");
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: ctrl });
    press(&ui, Key::Escape);
    assert_eq!(*log.borrow(), ["edit 0 x", "edit 1 true", "edit 2 y", "save", "back"]);
    assert_eq!(ui.get_query(), "", "typing never reached the search box");
}
