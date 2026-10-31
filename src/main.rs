use std::{
    cell::RefCell,
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, bail};
use ashpd::{
    WindowIdentifier,
    desktop::{
        PersistMode, Session,
        screencast::{CursorMode, Screencast, SourceType},
    },
};
use gst::prelude::*;
use gtk::{gdk, glib, prelude::*};

mod view;
use view::CastView;

// Closing a portal session releases the desktop's capture permission/indicator.
struct PortalSession(Option<Session<'static, Screencast<'static>>>);

impl Drop for PortalSession {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            glib::MainContext::default().spawn_local(async move {
                let _ = session.close().await;
            });
        }
    }
}

struct Capture {
    pipeline: gst::Pipeline,
    _session: PortalSession,
    // pipewiresrc borrows this descriptor; keep it alive until playback stops.
    _remote: OwnedFd,
    watch: Option<gst::bus::BusWatchGuard>,
    frames: Arc<AtomicU64>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

async fn capture(window: &gtk::ApplicationWindow) -> Result<(Capture, gdk::Paintable)> {
    let portal = Screencast::new()
        .await
        .context("Cannot connect to the ScreenCast portal")?;
    if !portal
        .available_source_types()
        .await?
        .contains(SourceType::Window)
    {
        bail!("Your desktop's ScreenCast portal does not support window capture.");
    }

    let session = PortalSession(Some(portal.create_session().await?));
    let handle = session.0.as_ref().unwrap();
    let cursor = if portal
        .available_cursor_modes()
        .await?
        .contains(CursorMode::Embedded)
    {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    portal
        .select_sources(
            handle,
            cursor,
            SourceType::Window.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await?
        .response()?;
    let parent = WindowIdentifier::from_native(window).await;
    let response = portal.start(handle, parent.as_ref()).await?.response()?;
    let stream = response
        .streams()
        .first()
        .context("No window was selected")?;
    let remote = portal.open_pipe_wire_remote(handle).await?;

    let source = gst::ElementFactory::make("pipewiresrc")
        .property("fd", remote.as_raw_fd())
        .property("path", stream.pipe_wire_node_id().to_string())
        .property("min-buffers", 8i32)
        .build()
        .context("Missing GStreamer pipewiresrc plugin")?;
    let frames = Arc::new(AtomicU64::new(0));
    let received = frames.clone();
    source
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            received.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    let (pipeline, paintable) = mirror_pipeline(&source)?;
    let capture = Capture {
        pipeline,
        _session: session,
        _remote: remote,
        watch: None,
        frames,
    };
    if let Err(error) = capture.pipeline.set_state(gst::State::Playing) {
        if let Some(message) = capture
            .pipeline
            .bus()
            .and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error]))
            && let gst::MessageView::Error(detail) = message.view()
        {
            bail!(
                "Cannot start the mirror: {} ({:?})",
                detail.error(),
                detail.debug()
            );
        }
        return Err(error).context("Cannot start the mirror");
    }
    Ok((capture, paintable))
}

fn mirror_pipeline(source: &gst::Element) -> Result<(gst::Pipeline, gdk::Paintable)> {
    let formats = gst::ElementFactory::make("capsfilter").build()?;
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 1u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .property_from_str("leaky", "downstream")
        .build()?;
    let sink = gst::ElementFactory::make("gtk4paintablesink")
        // Show the latest frame; clock/QoS scheduling can discard live portal
        // frames whose timestamps are based on the compositor's clock.
        .property("sync", false)
        .property("qos", false)
        .property("enable-last-sample", false)
        .build()
        .context("Missing GStreamer gtk4paintablesink plugin")?;
    let paintable = sink.property::<gdk::Paintable>("paintable");
    let convert = gst::ElementFactory::make("videoconvert").build()?;
    let pipeline = gst::Pipeline::new();
    let elements = [source, &formats, &queue, &convert, &sink];
    pipeline.add_many(elements)?;
    gst::Element::link_many(elements)?;

    if let Err(error) = pipeline.set_state(gst::State::Ready) {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(error).context("Cannot initialize video rendering");
    }
    let supported = sink.static_pad("sink").unwrap().query_caps(None);
    formats.set_property("caps", compatible_formats(&supported));
    Ok((pipeline, paintable))
}

// Compressed AMD DCC buffers are not reliably imported by every driver/plugin
// combination. Keep the renderer's other supported formats instead.
fn compatible_formats(caps: &gst::CapsRef) -> gst::Caps {
    if caps.is_any() {
        return caps.to_owned();
    }
    fn supported(format: &str) -> bool {
        let modifier = format
            .split_once(":0x")
            .and_then(|(_, hex)| u64::from_str_radix(hex, 16).ok())
            .unwrap_or(0);
        !(modifier >> 56 == 2 && modifier & (1 << 13) != 0)
    }

    let mut result = gst::Caps::new_empty();
    for (structure, features) in caps.iter_with_features() {
        let mut structure = structure.to_owned();
        if let Ok(list) = structure.get::<gst::List>("drm-format") {
            let formats: Vec<String> = list
                .iter()
                .filter_map(|v| v.get::<String>().ok())
                .filter(|f| supported(f))
                .collect();
            if formats.is_empty() {
                continue;
            }
            structure.set("drm-format", gst::List::new(formats));
        } else if let Ok(format) = structure.get::<String>("drm-format")
            && !supported(&format)
        {
            continue;
        }
        result
            .make_mut()
            .append_structure_full(structure, Some(features.to_owned()));
    }
    result
}

fn setup_appearance() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
    let css = gtk::CssProvider::new();
    css.load_from_data(
        ".mirror-window { background-color: #181818; color: #eeeeee; }
         .cast-toolbar { background: rgba(30, 30, 30, 0.96); color: #eeeeee; padding: 8px; }
         .mirror-window button { background: #333333; color: #eeeeee; border-color: #454545; }
         .mirror-window button:hover { background: #414141; }
         .mirror-window button:disabled { color: #888888; }
         .cast-toolbar button:checked { background: #365d8d; border-color: #5681b7; }
         .mirror-view { background-color: #101010; }
         .cast-toolbar scale { min-width: 100px; }
         .cast-toolbar label { color: #cccccc; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Screencast Mirror")
        .default_width(960)
        .default_height(600)
        .decorated(false)
        .build();
    window.add_css_class("mirror-window");
    let choose = gtk::Button::with_label("Choose window…");
    let stop = gtk::Button::with_label("Stop");
    stop.set_sensitive(false);
    let picture = CastView::new();
    let fill = gtk::ToggleButton::with_label("Fill");
    fill.set_active(true);
    fill.set_tooltip_text(Some("Fill crops the edges. Fit shows the whole target."));
    let pan = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    pan.set_value(50.0);
    pan.set_draw_value(false);
    pan.set_hexpand(true);
    pan.set_sensitive(false);
    pan.set_tooltip_text(Some("Crop position: left/top to right/bottom"));
    let fps = gtk::Label::new(None);
    fps.set_width_chars(7);
    fps.set_tooltip_text(Some(
        "Frames received from the compositor. Static or hidden targets may update less often.",
    ));
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.set_tooltip_text(Some("Close mirror"));
    close.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| window.close()
    ));
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.add_css_class("cast-toolbar");
    for widget in [
        choose.upcast_ref::<gtk::Widget>(),
        stop.upcast_ref(),
        fill.upcast_ref(),
        pan.upcast_ref(),
        fps.upcast_ref(),
        close.upcast_ref(),
    ] {
        toolbar.append(widget);
    }
    let bar = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(150)
        .reveal_child(true)
        .valign(gtk::Align::Start)
        .child(&toolbar)
        .build();
    fill.connect_toggled(glib::clone!(
        #[weak]
        picture,
        #[weak]
        pan,
        move |button| {
            picture.set_fill(button.is_active());
            pan.set_visible(button.is_active());
            button.set_label(if button.is_active() { "Fill" } else { "Fit" });
        }
    ));
    pan.connect_value_changed(glib::clone!(
        #[weak]
        picture,
        move |scale| {
            picture.set_pan(scale.value() / 100.0);
        }
    ));
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        bar,
        #[weak]
        picture,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape && picture.paintable().is_some() {
                bar.set_reveal_child(!bar.reveals_child());
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    ));
    window.add_controller(keys);
    let status = gtk::Label::new(Some("Choose a window to mirror."));
    status.set_wrap(true);
    status.set_halign(gtk::Align::Center);
    status.set_valign(gtk::Align::Center);
    status.set_margin_top(8);
    status.set_margin_bottom(8);
    status.set_margin_start(12);
    status.set_margin_end(12);
    let content = gtk::Overlay::new();
    content.add_css_class("mirror-view");
    content.set_child(Some(&picture));
    content.add_overlay(&status);
    content.add_overlay(&bar);
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    motion.connect_enter(glib::clone!(
        #[weak]
        bar,
        #[weak]
        picture,
        move |_, _, y| {
            reveal_toolbar_at(y, &bar, &picture);
        }
    ));
    motion.connect_motion(glib::clone!(
        #[weak]
        bar,
        #[weak]
        picture,
        move |_, _, y| {
            reveal_toolbar_at(y, &bar, &picture);
        }
    ));
    motion.connect_leave(glib::clone!(
        #[weak]
        bar,
        #[weak]
        picture,
        move |_| {
            bar.set_reveal_child(picture.paintable().is_none());
        }
    ));
    content.add_controller(motion);
    picture.connect_paintable_notify(glib::clone!(
        #[weak]
        status,
        #[weak]
        bar,
        #[weak]
        pan,
        #[weak]
        choose,
        move |picture| {
            let casting = picture.paintable().is_some();
            status.set_visible(!casting);
            bar.set_reveal_child(!casting);
            pan.set_sensitive(casting);
            choose.set_label(if casting {
                "Change target…"
            } else {
                "Choose window…"
            });
        }
    ));
    window.set_child(Some(&content));

    let active: Rc<RefCell<Option<Capture>>> = Rc::new(RefCell::new(None));
    let mut last_fps_update = Instant::now();
    glib::timeout_add_seconds_local(
        1,
        glib::clone!(
            #[weak]
            active,
            #[weak]
            fps,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                let elapsed = last_fps_update.elapsed().as_secs_f64();
                last_fps_update = Instant::now();
                if let Some(capture) = active.borrow().as_ref() {
                    let count = capture.frames.swap(0, Ordering::Relaxed);
                    fps.set_label(&format!("{:.0} fps", count as f64 / elapsed));
                } else {
                    fps.set_label("");
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    stop.connect_clicked(glib::clone!(
        #[strong]
        active,
        #[weak]
        picture,
        #[weak]
        status,
        move |button| {
            active.borrow_mut().take();
            picture.set_paintable(None::<&gdk::Paintable>);
            status.set_label("Mirror stopped.");
            button.set_sensitive(false);
        }
    ));

    choose.connect_clicked(glib::clone!(
        #[weak]
        window,
        #[weak]
        picture,
        #[weak]
        status,
        #[weak]
        stop,
        #[strong]
        active,
        move |button| {
            active.borrow_mut().take();
            picture.set_paintable(None::<&gdk::Paintable>);
            button.set_sensitive(false);
            stop.set_sensitive(false);
            status.set_label("Select a window in the desktop picker…");
            let button = button.clone();
            glib::MainContext::default().spawn_local(glib::clone!(
                #[strong]
                window,
                #[strong]
                picture,
                #[strong]
                status,
                #[strong]
                stop,
                #[strong]
                active,
                async move {
                    let result = capture(&window).await;
                    if !window.is_visible() {
                        return;
                    }
                    match result {
                        Ok((mut capture, paintable)) => {
                            let bus = capture.pipeline.bus().expect("Pipeline has a bus");
                            let pipeline = capture.pipeline.clone();
                            let watch = bus.add_watch_local(glib::clone!(
                                #[strong]
                                pipeline,
                                #[weak]
                                active,
                                #[weak]
                                picture,
                                #[weak]
                                status,
                                #[weak]
                                stop,
                                #[upgrade_or]
                                glib::ControlFlow::Break,
                                move |_, message| {
                                    let text = match message.view() {
                                        gst::MessageView::Error(error) => {
                                            eprintln!(
                                                "Capture failed: {} ({:?})",
                                                error.error(),
                                                error.debug()
                                            );
                                            Some(format!("Capture failed: {}", error.error()))
                                        }
                                        gst::MessageView::Eos(..) => Some(
                                            "The captured window or sharing session ended.".into(),
                                        ),
                                        _ => None,
                                    };
                                    if let Some(text) = text {
                                        // Remove the watch outside its own callback.
                                        glib::idle_add_local_once(glib::clone!(
                                            #[strong]
                                            pipeline,
                                            #[strong]
                                            active,
                                            #[strong]
                                            picture,
                                            #[strong]
                                            status,
                                            #[strong]
                                            stop,
                                            move || {
                                                if !active
                                                    .borrow()
                                                    .as_ref()
                                                    .is_some_and(|c| c.pipeline == pipeline)
                                                {
                                                    return;
                                                }
                                                active.borrow_mut().take();
                                                picture.set_paintable(None::<&gdk::Paintable>);
                                                status.set_label(&text);
                                                stop.set_sensitive(false);
                                            }
                                        ));
                                    }
                                    glib::ControlFlow::Continue
                                }
                            ));
                            match watch {
                                Ok(watch) => {
                                    capture.watch = Some(watch);
                                    *active.borrow_mut() = Some(capture);
                                    picture.set_paintable(Some(&paintable));
                                    status.set_label("Mirroring — resize this window as needed.");
                                    stop.set_sensitive(true);
                                }
                                Err(error) => {
                                    status.set_label(&format!("Cannot monitor capture: {error}"))
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("Could not start capture: {error:#}");
                            status.set_label(&format!("Could not start capture: {error:#}"))
                        }
                    }
                    button.set_sensitive(true);
                }
            ));
        }
    ));

    window.connect_close_request(move |_| {
        active.borrow_mut().take();
        glib::Propagation::Proceed
    });
    window.present();
}

fn reveal_toolbar_at(y: f64, bar: &gtk::Revealer, picture: &CastView) {
    let edge = if bar.reveals_child() {
        f64::from(bar.child().map_or(48, |child| child.height()).max(48)) + 12.0
    } else {
        24.0
    };
    bar.set_reveal_child(picture.paintable().is_none() || y <= edge);
}

fn main() -> glib::ExitCode {
    if let Err(error) = gst::init() {
        eprintln!("Cannot initialize GStreamer: {error}");
        return glib::ExitCode::FAILURE;
    }
    let app = gtk::Application::builder()
        .application_id("io.github.screencast_mirror")
        .build();
    app.connect_startup(|_| setup_appearance());
    app.connect_activate(build_ui);
    app.run()
}

#[cfg(test)]
mod tests {
    #[test]
    fn excludes_amd_dcc_but_preserves_other_formats() {
        gst::init().unwrap();
        let caps: gst::Caps = "video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=(string){ XR24:0x020000001056bb03, XR24:0x0200000010401b03, XR24, AR24:0x0100000000000001 }; video/x-raw,format=RGBA"
            .parse().unwrap();
        let filtered = super::compatible_formats(&caps);
        assert_eq!(filtered.size(), 2);
        let list = filtered
            .structure(0)
            .unwrap()
            .get::<gst::List>("drm-format")
            .unwrap();
        let formats: Vec<String> = list.iter().map(|v| v.get().unwrap()).collect();
        assert_eq!(
            formats,
            ["XR24:0x0200000010401b03", "XR24", "AR24:0x0100000000000001"]
        );
        assert!(filtered.features(0).unwrap().contains("memory:DMABuf"));
    }

    #[test]
    fn drops_structures_with_only_amd_dcc() {
        gst::init().unwrap();
        let caps: gst::Caps = "video/x-raw(memory:DMABuf),format=DMA_DRM,drm-format=XR24:0x020000001056bb03; video/x-raw,format=RGBA".parse().unwrap();
        let filtered = super::compatible_formats(&caps);
        assert_eq!(filtered.size(), 1);
        assert_eq!(
            filtered
                .structure(0)
                .unwrap()
                .get::<String>("format")
                .unwrap(),
            "RGBA"
        );
    }

    #[test]
    fn required_gstreamer_plugins_are_available() {
        gst::init().unwrap();
        for name in [
            "pipewiresrc",
            "queue",
            "videoconvert",
            "capsfilter",
            "gtk4paintablesink",
        ] {
            assert!(gst::ElementFactory::find(name).is_some(), "Missing {name}");
        }
    }
}
