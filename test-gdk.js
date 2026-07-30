import App from 'resource:///com/github/Aylur/ags/app.js';
import Widget from 'resource:///com/github/Aylur/ags/widget.js';
import GtkLayerShell from 'gi://GtkLayerShell';
import * as Utils from 'resource:///com/github/Aylur/ags/utils.js';

const win = Widget.Window({
    name: 'test-win',
    layer: 'overlay',
    anchor: ['top', 'left'],
    child: Widget.EventBox({
        css: 'background: red; min-width: 100px; min-height: 100px;',
        on_button_press_event: (self) => {
            console.log("Press");
            GtkLayerShell.set_anchor(win, GtkLayerShell.Edge.RIGHT, true);
            GtkLayerShell.set_anchor(win, GtkLayerShell.Edge.BOTTOM, true);
            return true;
        },
        on_button_release_event: (self) => {
            console.log("Release");
            GtkLayerShell.set_anchor(win, GtkLayerShell.Edge.RIGHT, false);
            GtkLayerShell.set_anchor(win, GtkLayerShell.Edge.BOTTOM, false);
            return true;
        }
    })
});

App.config({ windows: [win] });
