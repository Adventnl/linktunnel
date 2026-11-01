"""GNOME Wayland input bridge. The agent owns this process and its private pipes."""
import json, sys, time
from gi.repository import Gio, GLib
BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
RD = 'org.gnome.Mutter.RemoteDesktop'
SC = 'org.gnome.Mutter.ScreenCast'
def call(dest, path, interface, method, signature=None, args=()):
    return BUS.call_sync(dest, path, interface, method,
        GLib.Variant(signature, args) if signature else None,
        None, Gio.DBusCallFlags.NONE, 5000, None).unpack()

def main():
    session = call(RD, '/org/gnome/Mutter/RemoteDesktop', RD, 'CreateSession')[0]
    held_keys, held_buttons = set(), set()
    def notify(method, signature, args):
        call(RD, session, RD + '.Session', method, signature, args)
    try:
        session_id = call(RD, session, 'org.freedesktop.DBus.Properties', 'Get', '(ss)', (RD + '.Session', 'SessionId'))[0]
        sc = call(SC, '/org/gnome/Mutter/ScreenCast', SC, 'CreateSession', '(a{sv})', ({'remote-desktop-session-id': GLib.Variant('s', session_id)},))[0]
        _, physical, logical, _ = call('org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig', 'org.gnome.Mutter.DisplayConfig', 'GetCurrentState')
        streams = []
        for logical_monitor in logical:
            connector = logical_monitor[5][0][0]
            streams.append(call(SC, sc, SC + '.Session', 'RecordMonitor', '(sa{sv})', (connector, {}))[0])
        notify('Start', None, ())
        print('ready', flush=True)
        for line in sys.stdin:
            try:
                event = json.loads(line)
                op, args = event[0], event[1:]
                if op == 'abs':
                    monitor, x, y = args
                    notify('NotifyPointerMotionAbsolute', '(sdd)', (streams[monitor], float(x), float(y)))
                elif op == 'rel': notify('NotifyPointerMotionRelative', '(dd)', tuple(map(float,args)))
                elif op == 'button':
                    button, down = args
                    notify('NotifyPointerButton', '(ib)', (button, down))
                    (held_buttons.add if down else held_buttons.discard)(button)
                elif op == 'key':
                    code, down = args
                    notify('NotifyKeyboardKeycode', '(ub)', (code, down))
                    (held_keys.add if down else held_keys.discard)(code)
                elif op == 'scroll': notify('NotifyPointerAxis', '(ddu)', (float(args[0]), float(args[1]), 0))
                elif op == 'text':
                    def keysym(sym):
                        notify('NotifyKeyboardKeysym', '(ub)', (sym, True))
                        notify('NotifyKeyboardKeysym', '(ub)', (sym, False))
                    for c in args[0]:
                        if ord(c) > 127:
                            # GNOME's Unicode input sequence handles characters absent from
                            # the current physical keyboard map without replacing that map.
                            for code in (29, 42): notify('NotifyKeyboardKeycode', '(ub)', (code, True))
                            notify('NotifyKeyboardKeycode', '(ub)', (22, True))
                            notify('NotifyKeyboardKeycode', '(ub)', (22, False))
                            for code in (42, 29): notify('NotifyKeyboardKeycode', '(ub)', (code, False))
                            time.sleep(.05)  # Let the desktop IME enter Unicode composition.
                            hex_codes = dict(zip('1234567890abcdef', [2,3,4,5,6,7,8,9,10,11,30,48,46,32,18,33]))
                            for digit in format(ord(c), 'x'):
                                notify('NotifyKeyboardKeycode', '(ub)', (hex_codes[digit], True))
                                notify('NotifyKeyboardKeycode', '(ub)', (hex_codes[digit], False))
                            notify('NotifyKeyboardKeycode', '(ub)', (28, True))
                            notify('NotifyKeyboardKeycode', '(ub)', (28, False))
                            time.sleep(.05)  # Commit before the next text character.
                        else:
                            keysym({'\n':0xff0d, '\r':0xff0d, '\t':0xff09}.get(c, ord(c)))
                elif op == 'release':
                    for code in list(held_keys): notify('NotifyKeyboardKeycode', '(ub)', (code, False))
                    for button in list(held_buttons): notify('NotifyPointerButton', '(ib)', (button, False))
                    held_keys.clear(); held_buttons.clear()
                else: raise ValueError('Unknown input command')
                print('ok', flush=True)
            except Exception as e: print('error: '+str(e).replace('\n',' '), flush=True)
    finally:
        try:
            for code in held_keys: notify('NotifyKeyboardKeycode', '(ub)', (code, False))
            for button in held_buttons: notify('NotifyPointerButton', '(ib)', (button, False))
            notify('Stop', None, ())
        except GLib.Error: pass  # A closed compositor session already releases its devices.
if __name__ == '__main__': main()
