import Hyprland from 'resource:///com/github/Aylur/ags/service/hyprland.js';
Hyprland.messageAsync("cursorpos").then(res => {
    console.log("Hyprland IPC Result:", res);
}).catch(err => console.error("Hyprland IPC Error:", err));
