import App from 'resource:///com/github/Aylur/ags/app.js';
import Widget from 'resource:///com/github/Aylur/ags/widget.js';
import Variable from 'resource:///com/github/Aylur/ags/variable.js';
import * as Utils from 'resource:///com/github/Aylur/ags/utils.js';
import GtkLayerShell from 'gi://GtkLayerShell';

const CSS = App.configDir + '/style.css';
App.resetCss();
App.applyCss(CSS);

// Option 1: Dual-Window Smart Anchor State
const fabX = Variable(100);
const fabY = Variable(100);
const isOpen = Variable(false);

const DROPDOWN_W = 380;
const DROPDOWN_H = 480; 
const FAB_SIZE = 84; 

const screenW = Variable(1920);
const screenH = Variable(1080);

// Update screen bounds securely
function updateScreenBounds() {
    Utils.execAsync('hyprctl monitors -j').then(monitorOut => {
        const monitors = JSON.parse(monitorOut);
        const monitor = monitors.find(m => m.focused) || monitors[0];
        screenW.value = monitor.width / monitor.scale;
        screenH.value = monitor.height / monitor.scale;
    }).catch(err => console.error(err));
}
updateScreenBounds(); // Run once on startup

// The dropdown calculates its position relative to the FAB and screen boundaries in real-time
const dropdownX = Utils.derive([fabX, screenW], (x, w) => {
    let targetX = x; // Default: align left edges
    if (targetX + DROPDOWN_W > w) { // Strict collision detection right
        targetX = x - DROPDOWN_W + FAB_SIZE;
    }
    return targetX;
});

const dropdownY = Utils.derive([fabY, screenH], (y, h) => {
    let targetY = y + FAB_SIZE + 10; // Default: below the FAB
    if (targetY + DROPDOWN_H > h) { // Strict collision detection bottom
        targetY = y - DROPDOWN_H - 10;
    }
    return targetY;
});

let dragging = false;
let startX = 0;
let startY = 0;
let startFabX = 0;
let startFabY = 0;

// Helper: get the actual compositor position of the FAB window
function getFabScreenPos() {
    try {
        const layersJson = Utils.exec('hyprctl layers -j');
        const layers = JSON.parse(layersJson);
        // Search all monitors for our FAB
        for (const monitorKey of Object.keys(layers)) {
            const levels = layers[monitorKey].levels;
            for (const levelKey of Object.keys(levels)) {
                for (const surface of levels[levelKey]) {
                    if (surface.namespace === 'chathead-fab') {
                        return { x: surface.x, y: surface.y, w: surface.w, h: surface.h };
                    }
                }
            }
        }
    } catch (e) {
        console.error('getFabScreenPos error:', e);
    }
    return null;
}

const ChatHead = () => {
    return Widget.EventBox({
        class_name: 'fab-wrapper',
        setup: self => {
            self.add_events(32); 

            self.on('button-press-event', (box, event) => {
                const button = event.get_button()[1];
                if (button !== 1) return false;
                
                dragging = true;
                if (box._dragLoop) box._dragLoop = false; 
                
                updateScreenBounds(); 
                
                // Query BOTH the cursor AND the actual compositor window position synchronously.
                // This guarantees they use the exact same coordinate system (absolute screen pixels).
                try {
                    const pos = Utils.exec('hyprctl cursorpos').split(', ');
                    startX = parseInt(pos[0]);
                    startY = parseInt(pos[1]);
                    
                    // Get the real screen position from the compositor, NOT from our margin variables
                    const fabPos = getFabScreenPos();
                    
                    startFabX = fabX.value;
                    startFabY = fabY.value;
                    
                    console.log(`[DRAG_START] cursor=(${startX}, ${startY}) fabMargin=(${startFabX}, ${startFabY}) compositorPos=(${fabPos ? fabPos.x + ',' + fabPos.y : 'null'})`);
                } catch (e) {
                    console.error(e);
                    return true;
                }
                
                box._dragLoop = true;
                let _firstPoll = true;
                
                // Self-scheduling sequential async loop
                const pollCursor = () => {
                    if (!box._dragLoop || !dragging) return;
                    
                    Utils.execAsync('hyprctl cursorpos').then(currentOut => {
                        if (!box._dragLoop || !dragging) return;
                        
                        const currentPos = currentOut.split(', ');
                        const x = parseInt(currentPos[0]);
                        const y = parseInt(currentPos[1]);
                        
                        let newFabX = startFabX + (x - startX);
                        let newFabY = startFabY + (y - startY);
                        
                        // MATHEMATICAL CLAMPING: Prevent soft-lock by enforcing screen boundaries
                        newFabX = Math.max(0, Math.min(newFabX, screenW.value - FAB_SIZE));
                        newFabY = Math.max(0, Math.min(newFabY, screenH.value - FAB_SIZE));
                        
                        if (_firstPoll) {
                            console.log(`[FIRST_POLL] cursor=(${x}, ${y}) delta=(${x - startX}, ${y - startY}) newFab=(${newFabX}, ${newFabY}) oldFab=(${fabX.value}, ${fabY.value})`);
                            _firstPoll = false;
                        }
                        
                        fabX.value = newFabX;
                        fabY.value = newFabY;
                        
                        Utils.timeout(16, pollCursor);
                    }).catch(e => {
                        if (box._dragLoop && dragging) Utils.timeout(16, pollCursor);
                    });
                };
                
                pollCursor();

                return true;
            });

            self.on('button-release-event', (box, event) => {
                const button = event.get_button()[1];
                if (button !== 1) return false;
                
                dragging = false;
                box._dragLoop = false;
                
                Utils.execAsync('hyprctl cursorpos').then(out => {
                    const pos = out.split(', ');
                    const x = parseInt(pos[0]);
                    const y = parseInt(pos[1]);
                    const dist = Math.abs(x - startX) + Math.abs(y - startY);
                    
                    if (dist < 5) {
                        // It was a click, toggle the dropdown
                        if (!isOpen.value) {
                            updateScreenBounds(); 
                            isOpen.value = true;
                        } else {
                            isOpen.value = false;
                        }
                    } else {
                        // It was a drag, execute MAGNETIC EDGE SNAPPING
                        const screenMid = screenW.value / 2;
                        const targetX = fabX.value < screenMid ? 0 : (screenW.value - FAB_SIZE);
                        
                        let currentX = fabX.value;
                        const animateSnap = () => {
                            // Only animate if the user hasn't started dragging again
                            if (dragging) return; 
                            
                            // Simple ease-out animation formula
                            currentX += (targetX - currentX) * 0.25; 
                            
                            if (Math.abs(targetX - currentX) < 1) {
                                fabX.value = targetX; // Snap exactly to target
                            } else {
                                fabX.value = Math.round(currentX);
                                Utils.timeout(16, animateSnap);
                            }
                        };
                        animateSnap();
                    }
                }).catch(err => console.error(err));
                
                return true;
            });
        },
        child: Widget.Box({
            class_name: 'fab-button',
            child: Widget.Icon({
                class_name: 'fab-icon',
                icon: 'view-app-grid-symbolic', 
                size: 24,
            })
        })
    });
};

const DropdownCard = () => {
    return Widget.Box({
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
                            self.text = '';
                        }
                    })
                ]
            })
        ]
    });
};

export default {
    windows: [
        // FAB Window (Overlay layer: sits on top of absolutely everything including the dropdown)
        Widget.Window({
            name: 'chathead-fab',
            class_name: 'transparent-window',
            layer: 'overlay', 
            anchor: ['top', 'left'],
            setup: self => {
                // Defer initial margin until window is mapped to prevent silent failure
                Utils.idle(() => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.LEFT, fabX.value);
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.TOP, fabY.value);
                });
                
                fabX.connect('changed', () => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.LEFT, fabX.value);
                });
                fabY.connect('changed', () => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.TOP, fabY.value);
                });
            },
            child: ChatHead(),
        }),
        // Dropdown Window (Top layer: sits under the FAB so the FAB is never covered)
        Widget.Window({
            name: 'chathead-dropdown',
            class_name: 'transparent-window',
            layer: 'top', 
            anchor: ['top', 'left'],
            visible: isOpen.bind(),
            setup: self => {
                Utils.idle(() => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.LEFT, dropdownX.value);
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.TOP, dropdownY.value);
                });
                
                dropdownX.connect('changed', () => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.LEFT, dropdownX.value);
                });
                dropdownY.connect('changed', () => {
                    GtkLayerShell.set_margin(self, GtkLayerShell.Edge.TOP, dropdownY.value);
                });
            },
            child: DropdownCard(),
        })
    ],
};
