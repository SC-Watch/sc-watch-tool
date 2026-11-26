//! The overlay on Wayland: a wlr-layer-shell surface on the overlay layer.

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use super::Backend;
use crate::Drawn;

#[derive(Default)]
struct Output {
    /// The current mode: real pixels.
    mode: Option<(i32, i32)>,
    /// Its size in the compositor's logical units (xdg-output).
    logical: Option<(i32, i32)>,
}

#[derive(Default)]
struct State {
    outputs: Vec<Output>,
    configured: bool,
    closed: bool,
}

struct Surface {
    wl: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    viewport: Option<wp_viewport::WpViewport>,
    output: usize,
    /// Logical size, and the buffer's real size.
    size: (u32, u32),
    buffer: (usize, usize),
}

pub(crate) struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    viewporter: Option<wp_viewporter::WpViewporter>,
    outputs: Vec<wl_output::WlOutput>,
    screen: Option<(u32, u32)>,
    surface: Option<Surface>,
}

impl Wayland {
    pub(crate) fn open() -> Result<Wayland, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| format!("the registry: {e}"))?;
        let qh = queue.handle();
        let compositor: wl_compositor::WlCompositor = globals
            .bind(&qh, 1..=6, ())
            .map_err(|e| format!("wl_compositor: {e}"))?;
        let shm: wl_shm::WlShm = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("wl_shm: {e}"))?;
        let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 =
            globals.bind(&qh, 1..=4, ()).map_err(|_| {
                "this desktop has no layer shell for overlays (GNOME has none)".to_string()
            })?;
        let viewporter: Option<wp_viewporter::WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
        let xdg: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1> =
            globals.bind(&qh, 1..=3, ()).ok();
        let mut state = State::default();
        let mut outputs = Vec::new();
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" {
                let i = outputs.len();
                state.outputs.push(Output::default());
                let o: wl_output::WlOutput =
                    globals.registry().bind(g.name, g.version.min(4), &qh, i);
                if let Some(m) = &xdg {
                    m.get_xdg_output(&o, &qh, i);
                }
                outputs.push(o);
            }
        }
        // the outputs' modes and logical sizes
        for _ in 0..2 {
            queue
                .roundtrip(&mut state)
                .map_err(|e| format!("Wayland: {e}"))?;
        }
        if outputs.is_empty() {
            return Err("no monitors (wl_output)".into());
        }
        Ok(Wayland {
            conn,
            queue,
            qh,
            state,
            compositor,
            shm,
            layer_shell,
            viewporter,
            outputs,
            screen: None,
            surface: None,
        })
    }

    /// The monitor the captured screen is: the one of its size, else the
    /// first.
    fn output_for_screen(&self) -> usize {
        self.screen
            .and_then(|(w, h)| {
                self.state
                    .outputs
                    .iter()
                    .position(|o| o.mode == Some((w as i32, h as i32)))
            })
            .unwrap_or(0)
    }

    fn make_surface(
        &mut self,
        output: usize,
        lx: i32,
        ly: i32,
        size: (u32, u32),
        buffer: (usize, usize),
    ) -> Result<(), String> {
        let wl = self.compositor.create_surface(&self.qh, ());
        // clicks go through: an empty input region
        let region = self.compositor.create_region(&self.qh, ());
        wl.set_input_region(Some(&region));
        region.destroy();
        let layer = self.layer_shell.get_layer_surface(
            &wl,
            Some(&self.outputs[output]),
            zwlr_layer_shell_v1::Layer::Overlay,
            "sc-watch-mining".into(),
            &self.qh,
            (),
        );
        layer.set_anchor(zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left);
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.set_size(size.0, size.1);
        layer.set_margin(ly, 0, 0, lx);
        let viewport = if (size.0 as usize, size.1 as usize) != buffer {
            self.viewporter.as_ref().map(|v| {
                let vp = v.get_viewport(&wl, &self.qh, ());
                vp.set_destination(size.0 as i32, size.1 as i32);
                vp
            })
        } else {
            None
        };
        self.state.configured = false;
        wl.commit();
        for _ in 0..4 {
            if self.state.configured || self.state.closed {
                break;
            }
            self.queue
                .roundtrip(&mut self.state)
                .map_err(|e| format!("Wayland: {e}"))?;
        }
        if !self.state.configured {
            layer.destroy();
            wl.destroy();
            return Err("the compositor did not place the overlay".into());
        }
        self.surface = Some(Surface {
            wl,
            layer,
            viewport,
            output,
            size,
            buffer,
        });
        Ok(())
    }

    /// A buffer holding the label: shared memory the compositor reads.
    fn buffer(&self, d: &Drawn) -> Result<wl_buffer::WlBuffer, String> {
        let stride = d.width * 4;
        let len = stride * d.height;
        let fd = unsafe { libc::memfd_create(c"sc-watch-mining".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(format!("memfd: {}", std::io::Error::last_os_error()));
        }
        let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        file.write_all(&d.bgra).map_err(|e| format!("memfd: {e}"))?;
        let pool = self.shm.create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            d.width as i32,
            d.height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        Ok(buffer)
    }
}

impl Backend for Wayland {
    fn screen(&mut self, w: u32, h: u32) {
        self.screen = Some((w, h));
    }

    fn show(&mut self, d: &Drawn) -> Result<(), String> {
        if d.width == 0 || d.height == 0 {
            self.hide();
            return Ok(());
        }
        let oi = self.output_for_screen();
        let o = &self.state.outputs[oi];
        // a scaled monitor: placed and sized in logical units
        let f = match (o.mode, o.logical) {
            (Some((mw, _)), Some((lw, _))) if mw > 0 && lw > 0 && self.viewporter.is_some() => {
                lw as f64 / mw as f64
            }
            _ => 1.0,
        };
        let lx = (d.x as f64 * f).round() as i32;
        let ly = (d.y as f64 * f).round() as i32;
        let size = (
            ((d.width as f64 * f).round() as u32).max(1),
            ((d.height as f64 * f).round() as u32).max(1),
        );
        let fresh = self
            .surface
            .as_ref()
            .is_none_or(|s| s.output != oi || s.size != size || s.buffer != (d.width, d.height));
        if fresh {
            self.hide();
            self.make_surface(oi, lx, ly, size, (d.width, d.height))?;
        }
        let buffer = self.buffer(d)?;
        let s = self.surface.as_ref().expect("a surface");
        s.layer.set_margin(ly, 0, 0, lx);
        s.wl.attach(Some(&buffer), 0, 0);
        s.wl.damage(0, 0, i32::MAX, i32::MAX);
        s.wl.commit();
        self.conn.flush().map_err(|e| format!("Wayland: {e}"))
    }

    fn hide(&mut self) {
        if let Some(s) = self.surface.take() {
            s.layer.destroy();
            if let Some(vp) = s.viewport {
                vp.destroy();
            }
            s.wl.destroy();
            let _ = self.conn.flush();
        }
    }

    fn pump(&mut self) {
        let _ = self.conn.flush();
        if let Some(guard) = self.queue.prepare_read() {
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 0) } > 0 {
                let _ = guard.read();
            }
        }
        let _ = self.queue.dispatch_pending(&mut self.state);
        if self.state.closed {
            // the compositor took it away (the monitor went): made anew on
            // the next label
            self.state.closed = false;
            self.hide();
        }
    }

    fn describe(&self) -> &'static str {
        "Wayland (layer shell)"
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode {
            flags: WEnum::Value(flags),
            width,
            height,
            ..
        } = event
            && flags.contains(wl_output::Mode::Current)
        {
            state.outputs[*i].mode = Some((width, height));
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, height } = event {
            state.outputs[*i].logical = Some((width, height));
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                state.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // shown and done with: each label has a buffer of its own
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_region::WlRegion);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: ignore wp_viewporter::WpViewporter);
delegate_noop!(State: ignore wp_viewport::WpViewport);
delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
