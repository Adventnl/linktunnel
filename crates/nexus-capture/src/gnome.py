"""GNOME Wayland capture bridge. Stdout is a private framed pipe to the agent."""
import json, struct, sys, time
import gi
gi.require_version('Gst', '1.0')
from gi.repository import Gio, GLib, Gst

BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
DEST = 'org.gnome.Mutter.ScreenCast'
IFACE = DEST + '.Session'

def call(dest, path, interface, method, signature=None, args=()):
    return BUS.call_sync(dest, path, interface, method,
        GLib.Variant(signature, args) if signature else None,
        None, Gio.DBusCallFlags.NONE, 5000, None).unpack()

def monitors():
    _, physical, logical, _ = call('org.gnome.Mutter.DisplayConfig',
        '/org/gnome/Mutter/DisplayConfig', 'org.gnome.Mutter.DisplayConfig', 'GetCurrentState')
    result = []
    for x, y, scale, transform, primary, specs, _ in logical:
        for spec in specs[:1]:  # Mirrored outputs represent one logical screen.
            screen = next(p for p in physical if p[0] == spec)
            mode = next(m for m in screen[1] if m[6].get('is-current'))
            width, height = mode[1:3]
            if transform in (1, 3, 5, 7): width, height = height, width
            result.append(dict(index=len(result), name=spec[0], x=x, y=y,
                width=width, height=height, scale=scale, refresh_hz=mode[3], primary=primary))
    return result

def main():
    screens = monitors()
    if sys.argv[1] == 'monitors':
        print(json.dumps(screens)); return
    index = int(sys.argv[1])
    screen = screens[index]
    session = call(DEST, '/org/gnome/Mutter/ScreenCast', DEST, 'CreateSession', '(a{sv})', ({},))[0]
    pipeline = None
    try:
        stream = call(DEST, session, IFACE, 'RecordMonitor', '(sa{sv})',
            (screen['name'], {'cursor-mode': GLib.Variant('u', 1)}))[0]
        nodes = []
        BUS.signal_subscribe(DEST, DEST + '.Stream', 'PipeWireStreamAdded', stream, None,
            Gio.DBusSignalFlags.NONE, lambda *args: nodes.append(args[5].unpack()[0]))
        call(DEST, session, IFACE, 'Start')
        ctx = GLib.MainContext.default()
        deadline = time.monotonic() + 10
        while not nodes:
            while ctx.pending(): ctx.iteration(False)
            if time.monotonic() > deadline: raise RuntimeError('PipeWire stream startup timed out')
            time.sleep(.01)
        Gst.init(None)
        pipeline = Gst.parse_launch(f'pipewiresrc path={nodes[0]} always-copy=true do-timestamp=true ! videoconvert ! video/x-raw,format=BGRA ! appsink name=frames max-buffers=1 drop=true sync=false')
        sink = pipeline.get_by_name('frames')
        pipeline.set_state(Gst.State.PLAYING)
        print(json.dumps(screen), flush=True)
        out = sys.stdout.buffer
        for line in sys.stdin:
            timeout = min(max(int(line), 0), 2000)
            sample = sink.emit('try-pull-sample', timeout * Gst.MSECOND)
            if sample is None:
                message = pipeline.get_bus().pop_filtered(Gst.MessageType.ERROR)
                if message: raise RuntimeError(str(message.parse_error()))
                out.write(struct.pack('<IIII', 0, 0, 0, 0)); out.flush(); continue
            caps = sample.get_caps().get_structure(0)
            width, height = caps.get_value('width'), caps.get_value('height')
            buf = sample.get_buffer()
            ok, mapped = buf.map(Gst.MapFlags.READ)
            if not ok: raise RuntimeError('Cannot map PipeWire frame')
            try:
                data = mapped.data
                if len(data) % height: raise RuntimeError('Invalid video buffer stride')
                out.write(struct.pack('<IIII', width, height, len(data)//height, len(data)))
                out.write(data); out.flush()
            finally: buf.unmap(mapped)
    finally:
        if pipeline: pipeline.set_state(Gst.State.NULL)
        call(DEST, session, IFACE, 'Stop')

if __name__ == '__main__': main()
