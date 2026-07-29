import App from 'resource:///com/github/Aylur/ags/app.js';
import Widget from 'resource:///com/github/Aylur/ags/widget.js';
import Variable from 'resource:///com/github/Aylur/ags/variable.js';

const CSS = App.configDir + '/style.css';
App.resetCss();
App.applyCss(CSS);

// State
const posX = Variable(100);
const posY = Variable(100);
const isOpen = Variable(false);
const isListening = Variable(false);

// Dragging state (local variables inside the setup closure are fine)
let dragging = false;
let startX = 0;
let startY = 0;
let initialPosX = 0;
let initialPosY = 0;

const ChatHead = () => {
    // The Floating Circle
    const fab = Widget.EventBox({
        class_name: 'fab-wrapper',
        setup: self => {
            // Allow receiving motion events
            self.add_events(32); // Gdk.EventMask.POINTER_MOTION_MASK

            self.on('button-press-event', (box, event) => {
                if (event.get_button()[1] !== 1) return false; // Only left click
                dragging = true;
                const [_, x, y] = event.get_root_coords();
                startX = x;
                startY = y;
                initialPosX = posX.value;
                initialPosY = posY.value;
                return true;
            });

            self.on('motion-notify-event', (box, event) => {
                if (!dragging) return false;
                const [_, x, y] = event.get_root_coords();
                const dx = x - startX;
                const dy = y - startY;
                posX.value = initialPosX + dx;
                posY.value = initialPosY + dy;
                return true;
            });

            self.on('button-release-event', (box, event) => {
                if (event.get_button()[1] !== 1) return false;
                dragging = false;
                
                // If we didn't drag far, treat it as a click to toggle
                const [_, x, y] = event.get_root_coords();
                const dist = Math.abs(x - startX) + Math.abs(y - startY);
                if (dist < 5) {
                    isOpen.value = !isOpen.value;
                }
                return true;
            });
        },
        child: Widget.Box({
            class_name: 'fab-button',
            child: Widget.Icon({
                class_name: 'fab-icon',
                icon: 'view-app-grid-symbolic', // A standard GTK icon for now
                size: 24,
            })
        })
    });

    // The Dropdown Card (conditionally visible)
    const dropdown = Widget.Revealer({
        reveal_child: isOpen.bind(),
        transition_duration: 300,
        transition: 'slide_down',
        child: Widget.Box({
            class_name: 'dropdown-card',
            vertical: true,
            children: [
                Widget.Box({
                    class_name: 'card-header',
                    children: [
                        Widget.Label({ class_name: 'header-title', label: 'ChatHead AI' }),
                        Widget.Box({ hexpand: true }),
                        Widget.Label({ class_name: 'status-pill', label: 'CLI Ready' })
                    ]
                }),
                Widget.Scrollable({
                    class_name: 'chat-body',
                    vexpand: true,
                    child: Widget.Box({
                        vertical: true,
                        children: [
                            Widget.Label({ class_name: 'chat-message ai', label: "Hello! I'm your OS-level ChatHead. Drag me anywhere!", wrap: true }),
                            Widget.Label({ class_name: 'chat-message user', label: "Testing AGS...", wrap: true, xalign: 1 })
                        ]
                    })
                }),
                Widget.Box({
                    class_name: 'chat-footer',
                    children: [
                        Widget.Entry({
                            class_name: 'chat-input',
                            placeholder_text: 'Type a prompt...',
                            hexpand: true,
                            on_accept: (self) => {
                                // Add send logic later
                                self.text = '';
                            }
                        })
                    ]
                })
            ]
        })
    });

    return Widget.Box({
        vertical: true,
        class_name: 'app-container',
        children: [
            // Align the FAB to the right side of the layout container
            Widget.Box({
                halign: 'end',
                child: fab
            }),
            dropdown
        ]
    });
};

export default {
    windows: [
        Widget.Window({
            name: 'chathead-ai',
            class_name: 'transparent-window',
            layer: 'overlay', // Completely ignores window manager
            anchor: ['top', 'left'],
            margins: Utils.derive([posX, posY], (x, y) => [y, 0, 0, x]), // Bind to coordinates
            child: ChatHead(),
        })
    ],
};
