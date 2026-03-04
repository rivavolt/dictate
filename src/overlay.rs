use anyhow::Result;
use cosmic_text::{Attrs, Buffer, Color, FontSystem, Metrics, Shaping, SwashCache};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

const BAR_HEIGHT: u32 = 48;
const FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 20.0;
const PADDING_X: i32 = 16;

pub enum Command {
    Show,
    Hide,
    SetText(String),
    Shutdown,
}

#[derive(Clone)]
pub struct Handle {
    tx: calloop::channel::Sender<Command>,
}

impl Handle {
    pub fn show(&self) {
        let _ = self.tx.send(Command::Show);
    }

    pub fn hide(&self) {
        let _ = self.tx.send(Command::Hide);
    }

    pub fn set_text(&self, text: String) {
        let _ = self.tx.send(Command::SetText(text));
    }
}

pub fn spawn() -> Result<Handle> {
    let (tx, rx) = calloop::channel::channel::<Command>();
    let handle = Handle { tx };

    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(move || {
            if let Err(e) = run(rx) {
                tracing::error!("overlay thread: {e}");
            }
        })?;

    Ok(handle)
}

fn run(cmd_rx: calloop::channel::Channel<Command>) -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("dictate-overlay"),
        None,
    );

    layer.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, BAR_HEIGHT);
    layer.set_exclusive_zone(0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();

    let pool = SlotPool::new(256, &shm)?;
    let mut font_system = FontSystem::new();
    let swash_cache = SwashCache::new();
    let text_buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        font_system,
        swash_cache,
        text_buffer,
        text: String::new(),
        visible: false,
        configured: false,
        width: 0,
        height: BAR_HEIGHT,
        exit: false,
    };

    let mut event_loop = calloop::EventLoop::<State>::try_new()?;
    let loop_handle = event_loop.handle();

    let wayland_source = calloop_wayland_source::WaylandSource::new(conn, event_queue);
    loop_handle
        .insert_source(wayland_source, |_, queue, state| {
            queue.dispatch_pending(state)
        })
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;

    loop_handle.insert_source(cmd_rx, |event, _, state| {
        if let calloop::channel::Event::Msg(cmd) = event {
            match cmd {
                Command::Show => {
                    state.visible = true;
                    state.text = "Recording...".into();
                    state.redraw();
                }
                Command::Hide => {
                    state.visible = false;
                    state.redraw();
                }
                Command::SetText(text) => {
                    state.text = text;
                    if state.visible {
                        state.redraw();
                    }
                }
                Command::Shutdown => {
                    state.exit = true;
                }
            }
        }
    }).map_err(|e| anyhow::anyhow!("cmd channel: {e}"))?;

    while !state.exit {
        event_loop.dispatch(std::time::Duration::from_millis(100), &mut state)?;
    }

    Ok(())
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    font_system: FontSystem,
    swash_cache: SwashCache,
    text_buffer: Buffer,
    text: String,
    visible: bool,
    configured: bool,
    width: u32,
    height: u32,
    exit: bool,
}

impl State {
    fn redraw(&mut self) {
        if self.width == 0 || !self.configured {
            return;
        }

        let width = self.width;
        let height = self.height;
        let stride = width as i32 * 4;

        let (buffer, canvas) = self
            .pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        if !self.visible {
            canvas.fill(0);
        } else {
            // Semi-transparent dark background (pre-multiplied ARGB)
            let alpha: u32 = 0xCC;
            let r = (0x1E * alpha) / 255;
            let g = (0x1E * alpha) / 255;
            let b = (0x2E * alpha) / 255;
            let bg = (alpha << 24) | (r << 16) | (g << 8) | b;
            let bg_bytes = bg.to_ne_bytes();
            canvas
                .chunks_exact_mut(4)
                .for_each(|chunk| chunk.copy_from_slice(&bg_bytes));

            // Render text
            self.text_buffer.set_text(
                &mut self.font_system,
                &self.text,
                &Attrs::new(),
                Shaping::Advanced,
            );
            self.text_buffer.set_size(
                &mut self.font_system,
                Some((width as i32 - PADDING_X * 2) as f32),
                Some(height as f32),
            );
            self.text_buffer
                .shape_until_scroll(&mut self.font_system, false);

            let cw = width as i32;
            let ch = height as i32;
            let pad_y = ((height as i32 - LINE_HEIGHT as i32) / 2).max(0);

            self.text_buffer.draw(
                &mut self.font_system,
                &mut self.swash_cache,
                Color::rgb(0xFF, 0xFF, 0xFF),
                |x, y, w, h, color| {
                    let x = x + PADDING_X;
                    let y = y + pad_y;
                    let a = color.a() as u32;
                    if a == 0 {
                        return;
                    }
                    let pr = (color.r() as u32 * a) / 255;
                    let pg = (color.g() as u32 * a) / 255;
                    let pb = (color.b() as u32 * a) / 255;
                    let pixel = ((a << 24) | (pr << 16) | (pg << 8) | pb).to_ne_bytes();

                    for row in y..(y + h as i32).min(ch) {
                        if row < 0 {
                            continue;
                        }
                        for col in x..(x + w as i32).min(cw) {
                            if col < 0 {
                                continue;
                            }
                            let off = (row * cw + col) as usize * 4;
                            if off + 4 <= canvas.len() {
                                if a >= 255 {
                                    canvas[off..off + 4].copy_from_slice(&pixel);
                                } else {
                                    let inv = 255 - a;
                                    for i in 0..4 {
                                        canvas[off + i] = ((pixel[i] as u32 * a
                                            + canvas[off + i] as u32 * inv)
                                            / 255)
                                            as u8;
                                    }
                                }
                            }
                        }
                    }
                },
            );
        }

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(self.layer.wl_surface()).expect("attach");
        self.layer.commit();
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32,
    ) {}
    fn transform_changed(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {}
    fn surface_leave(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface,
        configure: LayerSurfaceConfigure, _: u32,
    ) {
        self.width = configure.new_size.0.max(1);
        self.height = configure.new_size.1.max(BAR_HEIGHT);
        self.configured = true;
        self.redraw();
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}
