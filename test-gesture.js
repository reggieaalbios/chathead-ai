imports.gi.versions.Gtk = '3.0';
const { Gtk, Gdk } = imports.gi;
Gtk.init(null);
const win = new Gtk.Window({ title: "Test", default_width: 200, default_height: 200 });
const box = new Gtk.EventBox();
win.add(box);
const drag = new Gtk.GestureDrag({ widget: box });
drag.connect("drag-begin", (gesture, start_x, start_y) => {
    print("Drag Begin", start_x, start_y);
});
drag.connect("drag-update", (gesture, offset_x, offset_y) => {
    print("Drag Update", offset_x, offset_y);
});
drag.connect("drag-end", (gesture, offset_x, offset_y) => {
    print("Drag End", offset_x, offset_y);
    Gtk.main_quit();
});
win.show_all();
// GTK needs a main loop
// Gtk.main(); // We can't run this headless and click it easily
