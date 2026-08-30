/* global global */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const READINESS_NAME = 'io.github.chathead_ai.ChatHead.GnomePresentation';
const READINESS_PATH = '/io/github/chathead_ai/ChatHead/GnomePresentation';
const SIDECAR_NAME = 'io.github.chathead_ai.ChatHead.Sidecar';
const PRESENTATION_PATH = '/io/github/chathead_ai/ChatHead/Presentation';
const PRESENTATION_IFACE = 'io.github.chathead_ai.ChatHead.Presentation1';

const READINESS_XML = `<node><interface name="io.github.chathead_ai.ChatHead.GnomePresentation1">
  <method name="GetReadiness"><arg type="s" name="readiness" direction="out"/></method>
</interface></node>`;

const PRESENTATION_XML = `<node><interface name="${PRESENTATION_IFACE}">
  <method name="GetPresentationSnapshot"><arg type="s" name="snapshot" direction="out"/></method>
  <method name="GetAttachment"><arg type="s" name="attachment_id" direction="in"/><arg type="s" name="mime_type" direction="out"/><arg type="ay" name="bytes" direction="out"/></method>
  <method name="TogglePanel"/><method name="Send"><arg type="s" name="text" direction="in"/></method>
  <method name="StopResponse"/><method name="Retry"><arg type="s" name="message_id" direction="in"/></method>
  <method name="NewChat"/><method name="ActivateVoice"/><method name="CancelVoice"/>
  <method name="OpenSettings"/><method name="ConfirmLink"><arg type="b" name="open" direction="in"/></method>
  <method name="RequestLink"><arg type="s" name="destination" direction="in"/></method>
  <method name="CopyResponse"><arg type="s" name="message_id" direction="in"/><arg type="s" name="format" direction="in"/></method>
  <method name="StopOverlay"/>
  <signal name="PresentationChanged"><arg type="t" name="revision"/><arg type="s" name="patch"/></signal>
</interface></node>`;

export default class ChatHeadExtension extends Extension {
    enable() {
        this._signals = [];
        this._timeouts = new Set();
        this._revision = 0;
        this._snapshot = null;
        this._dragOrigin = null;
        this._suppressNextClick = false;
        this._buildActors();
        this._exportReadiness();
        this._connectShellSignals();
        this._connectSidecar();
        this._reposition();
        this._syncShellVisibility();
    }

    disable() {
        if (this._proxy) {
            try {
                this._proxy.call_sync('StopOverlay', null, Gio.DBusCallFlags.NONE, 500, null);
            } catch {
                // The sidecar may already be gone; actor teardown is still required.
            }
        }
        for (const [object, id] of this._signals ?? [])
            object.disconnect(id);
        for (const id of this._timeouts ?? [])
            GLib.source_remove(id);
        this._signals = [];
        this._timeouts?.clear();
        this._proxy = null;
        this._readinessObject?.unexport();
        this._readinessObject = null;
        if (this._nameOwnerId)
            Gio.bus_unown_name(this._nameOwnerId);
        this._nameOwnerId = 0;
        this._root?.destroy();
        this._root = null;
        this._orb = null;
        this._panel = null;
        this._dragOrigin = null;
        this._suppressNextClick = false;
    }

    GetReadiness() {
        return JSON.stringify({
            protocolVersion: 1,
            gnomeVersion: '46',
            capabilities: [
                'topChrome', 'fullscreen', 'workspaces', 'overviewHiding', 'lockHiding',
                'drag', 'focus', 'structuredMarkdown', 'copy', 'safeLinks', 'voice'
            ],
        });
    }

    _buildActors() {
        this._root = new St.Widget({
            name: 'chathead-root', reactive: false, layout_manager: new Clutter.FixedLayout(),
        });
        const orbIcon = new St.Icon({
            gicon: new Gio.FileIcon({file: this.dir.get_child('chathead-orb.svg')}),
            icon_size: 84,
        });
        this._orb = new St.Button({
            style_class: 'chathead-orb', child: orbIcon, reactive: true, can_focus: true,
            accessible_name: 'Toggle ChatHead panel',
        });
        this._panel = new St.BoxLayout({
            style_class: 'chathead-panel', vertical: true, reactive: true, visible: false,
        });
        this._root.add_child(this._panel);
        this._root.add_child(this._orb);
        Main.layoutManager.addTopChrome(this._root, {trackFullscreen: true});

        this._signals.push([this._orb, this._orb.connect('clicked', () => {
            if (!this._suppressNextClick)
                this._call('TogglePanel');
        })]);
        this._signals.push([this._orb, this._orb.connect('button-press-event', (_actor, event) => {
            if (event.get_button() !== 1)
                return Clutter.EVENT_PROPAGATE;
            this._beginDrag(event);
            return Clutter.EVENT_PROPAGATE;
        })]);
        this._signals.push([this._orb, this._orb.connect('touch-event', (_actor, event) => {
            if (event.type() === Clutter.EventType.TOUCH_BEGIN)
                this._beginDrag(event);
            return Clutter.EVENT_PROPAGATE;
        })]);
        this._signals.push([global.stage, global.stage.connect('captured-event', (_stage, event) =>
            this._handleCapturedDragEvent(event))]);
    }

    _beginDrag(event) {
        const [pointerX, pointerY] = event.get_coords();
        this._dragOrigin = {
            pointerX,
            pointerY,
            orbX: this._orb.x,
            orbY: this._orb.y,
            sequence: event.get_event_sequence(),
        };
        this._suppressNextClick = false;
    }

    _handleCapturedDragEvent(event) {
        if (!this._dragOrigin)
            return Clutter.EVENT_PROPAGATE;

        const type = event.type();
        const isPointerMotion = type === Clutter.EventType.MOTION;
        const isTouchMotion = type === Clutter.EventType.TOUCH_UPDATE &&
            event.get_event_sequence() === this._dragOrigin.sequence;
        if (isPointerMotion || isTouchMotion) {
            const [pointerX, pointerY] = event.get_coords();
            const deltaX = pointerX - this._dragOrigin.pointerX;
            const deltaY = pointerY - this._dragOrigin.pointerY;
            this._suppressNextClick ||= Math.hypot(deltaX, deltaY) > 5;
            this._orb.set_position(
                this._dragOrigin.orbX + deltaX,
                this._dragOrigin.orbY + deltaY);
            this._positionPanel();
            return this._suppressNextClick ? Clutter.EVENT_STOP : Clutter.EVENT_PROPAGATE;
        }

        const isPointerEnd = type === Clutter.EventType.BUTTON_RELEASE;
        const isTouchEnd = (type === Clutter.EventType.TOUCH_END ||
            type === Clutter.EventType.TOUCH_CANCEL) &&
            event.get_event_sequence() === this._dragOrigin.sequence;
        if (!isPointerEnd && !isTouchEnd)
            return Clutter.EVENT_PROPAGATE;

        this._dragOrigin = null;
        this._clampOrb();
        const id = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._timeouts.delete(id);
            this._suppressNextClick = false;
            return GLib.SOURCE_REMOVE;
        });
        this._timeouts.add(id);
        return this._suppressNextClick ? Clutter.EVENT_STOP : Clutter.EVENT_PROPAGATE;
    }

    _exportReadiness() {
        this._readinessObject = Gio.DBusExportedObject.wrapJSObject(READINESS_XML, this);
        this._readinessObject.export(Gio.DBus.session, READINESS_PATH);
        this._nameOwnerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, READINESS_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    _connectShellSignals() {
        this._signals.push([Main.overview, Main.overview.connect('showing', () => this._root?.hide())]);
        this._signals.push([Main.overview, Main.overview.connect('hidden', () => this._syncShellVisibility())]);
        this._signals.push([Main.sessionMode, Main.sessionMode.connect('updated', () => this._syncShellVisibility())]);
        this._signals.push([Main.layoutManager, Main.layoutManager.connect('monitors-changed', () => this._reposition())]);
    }

    _connectSidecar() {
        try {
            this._proxy = Gio.DBusProxy.new_for_bus_sync(
                Gio.BusType.SESSION, Gio.DBusProxyFlags.DO_NOT_AUTO_START,
                Gio.DBusNodeInfo.new_for_xml(PRESENTATION_XML).interfaces[0], SIDECAR_NAME,
                PRESENTATION_PATH, PRESENTATION_IFACE, null);
            this._signals.push([this._proxy, this._proxy.connect('notify::g-name-owner', () => {
                if (this._proxy.get_name_owner()) this._resync();
                else this._handleSidecarLoss();
            })]);
            this._signals.push([this._proxy, this._proxy.connect('g-signal', (_proxy, _sender, signal, parameters) => {
                if (signal !== 'PresentationChanged') return;
                const [revision, patchJson] = parameters.deep_unpack();
                const revisionNumber = Number(revision);
                if (this._revision !== 0 && revisionNumber !== this._revision + 1) {
                    this._resync();
                    return;
                }
                try {
                    const patch = JSON.parse(patchJson);
                    if (patch.kind !== 'snapshot') throw new Error('unsupported patch');
                    this._applySnapshot(patch.snapshot);
                } catch {
                    this._resync();
                }
            })]);
            if (this._proxy.get_name_owner()) this._resync();
        } catch {
            this._handleSidecarLoss();
        }
    }

    _resync() {
        try {
            const reply = this._proxy.call_sync('GetPresentationSnapshot', null, Gio.DBusCallFlags.NONE, 1000, null);
            this._applySnapshot(JSON.parse(reply.deep_unpack()[0]));
        } catch {
            this._handleSidecarLoss();
        }
    }

    _applySnapshot(snapshot) {
        if (snapshot.protocolVersion !== 1) {
            this._handleSidecarLoss();
            return;
        }
        this._snapshot = snapshot;
        this._revision = Number(snapshot.revision);
        this._root.visible = Boolean(snapshot.visible);
        this._panel.visible = Boolean(snapshot.visible && snapshot.panelOpen);
        if (snapshot.dimensions)
            this._panel.set_size(snapshot.dimensions.width, snapshot.dimensions.height);
        this._renderPanel(snapshot);
        this._positionPanel();
        this._syncShellVisibility();
    }

    _renderPanel(snapshot) {
        const draft = this._entry?.get_text() ?? '';
        while (this._panel.get_first_child())
            this._panel.get_first_child().destroy();
        const header = new St.BoxLayout({style_class: 'chathead-header'});
        header.add_child(new St.Label({text: snapshot.busy ? 'ChatHead · Thinking' : 'ChatHead'}));
        const newChat = new St.Button({label: 'New chat', can_focus: true});
        newChat.connect('clicked', () => this._call('NewChat'));
        header.add_child(newChat);
        this._panel.add_child(header);

        const scroll = new St.ScrollView({style_class: 'chathead-transcript', overlay_scrollbars: true});
        const transcript = new St.BoxLayout({vertical: true, x_expand: true});
        for (const message of snapshot.conversation ?? [])
            transcript.add_child(this._messageActor(message));
        scroll.set_child(transcript);
        this._panel.add_child(scroll);

        if (snapshot.message && (snapshot.conversation?.length ?? 0) === 0)
            this._panel.add_child(new St.Label({text: snapshot.message}));
        if (snapshot.failure)
            this._panel.add_child(new St.Label({style_class: 'chathead-error', text: snapshot.failure}));
        if (snapshot.pendingLinkConfirmation)
            this._panel.add_child(this._linkConfirmation(snapshot.pendingLinkConfirmation));

        const composer = new St.BoxLayout({style_class: 'chathead-composer'});
        this._entry = new St.Entry({hint_text: 'Message ChatHead…', can_focus: true, x_expand: true});
        this._entry.set_text(snapshot.composerText || draft);
        this._entry.clutter_text.connect('activate', () => this._send());
        const send = new St.Button({label: snapshot.busy ? '■' : '↑', can_focus: true});
        send.connect('clicked', () => snapshot.busy ? this._call('StopResponse') : this._send());
        composer.add_child(this._entry);
        composer.add_child(send);
        this._panel.add_child(composer);
    }

    _messageActor(message) {
        const box = new St.BoxLayout({vertical: true, style_class: `chathead-message ${message.role}`});
        if (message.role === 'assistant' && message.document)
            for (const block of message.document.blocks ?? []) box.add_child(this._blockActor(block));
        else
            box.add_child(this._selectableLabel(message.text));
        const actions = new St.BoxLayout({style_class: 'chathead-actions'});
        for (const [label, format] of [['Copy', 'plainText'], ['Markdown', 'markdown']]) {
            const button = new St.Button({label, can_focus: true});
            button.connect('clicked', () => this._call('CopyResponse', new GLib.Variant('(ss)', [message.id, format])));
            actions.add_child(button);
        }
        if (message.role === 'assistant' && message.state !== 'streaming') {
            const retry = new St.Button({label: 'Retry', can_focus: true});
            retry.connect('clicked', () => this._call('Retry', new GLib.Variant('(s)', [message.id])));
            actions.add_child(retry);
        }
        box.add_child(actions);
        return box;
    }

    _blockActor(block) {
        if (block.kind === 'code')
            return this._selectableLabel(block.content.text, 'chathead-code');
        if (block.kind === 'separator')
            return new St.Widget({style_class: 'chathead-separator'});
        if (block.kind === 'heading')
            return this._inlineActor(block.content.spans, `chathead-heading h${block.content.level}`);
        if (block.kind === 'paragraph')
            return this._inlineActor(block.content, 'chathead-paragraph');
        if (block.kind === 'list') {
            const list = new St.BoxLayout({vertical: true});
            for (const item of block.content.items ?? []) {
                const row = new St.BoxLayout();
                row.add_child(new St.Label({text: item.checked === true ? '☑ ' : item.checked === false ? '☐ ' : '• '}));
                for (const child of item.blocks ?? []) row.add_child(this._blockActor(child));
                list.add_child(row);
            }
            return list;
        }
        if (block.kind === 'table') {
            const table = new St.BoxLayout({vertical: true, style_class: 'chathead-table'});
            for (const row of [block.content.header, ...(block.content.rows ?? [])]) {
                const actor = new St.BoxLayout();
                for (const cell of row ?? []) actor.add_child(this._inlineActor(cell, 'chathead-table-cell'));
                table.add_child(actor);
            }
            return table;
        }
        return this._selectableLabel(this._blockText(block));
    }

    _inlineActor(spans, styleClass) {
        const row = new St.BoxLayout({style_class: styleClass});
        for (const span of spans ?? []) {
            if (span.kind === 'link') {
                const button = new St.Button({label: this._inlineText(span.content.label), can_focus: true});
                button.connect('clicked', () => this._call('RequestLink', new GLib.Variant('(s)', [span.content.destination])));
                row.add_child(button);
            } else row.add_child(this._selectableLabel(this._inlineText([span])));
        }
        return row;
    }

    _inlineText(spans) {
        return (spans ?? []).map(span => {
            if (typeof span.content === 'string') return span.content;
            if (Array.isArray(span.content)) return this._inlineText(span.content);
            if (span.kind === 'softBreak' || span.kind === 'hardBreak') return '\n';
            if (span.content?.label) return this._inlineText(span.content.label);
            return '';
        }).join('');
    }

    _blockText(block) {
        if (typeof block.content === 'string') return block.content;
        return block.content?.text ?? '';
    }

    _selectableLabel(text, styleClass = '') {
        const label = new St.Label({text: text ?? '', style_class: styleClass, x_expand: true});
        label.clutter_text.line_wrap = true;
        label.clutter_text.selectable = true;
        return label;
    }

    _linkConfirmation(destination) {
        const row = new St.BoxLayout({style_class: 'chathead-link-confirmation'});
        row.add_child(this._selectableLabel(`Open this link? ${destination}`));
        for (const [label, open] of [['Cancel', false], ['Open', true]]) {
            const button = new St.Button({label, can_focus: true});
            button.connect('clicked', () => this._call('ConfirmLink', new GLib.Variant('(b)', [open])));
            row.add_child(button);
        }
        return row;
    }

    _send() {
        const text = this._entry?.get_text().trim();
        if (!text) return;
        this._call('Send', new GLib.Variant('(s)', [text]));
        this._entry.set_text('');
    }

    _call(method, parameters = null) {
        try {
            this._proxy?.call(method, parameters, Gio.DBusCallFlags.NONE, 3000, null, null);
        } catch {
            this._handleSidecarLoss();
        }
    }

    _handleSidecarLoss() {
        this._revision = 0;
        this._snapshot = null;
        if (this._root) this._root.visible = false;
    }

    _syncShellVisibility() {
        if (!this._root) return;
        const sessionUnavailable = Main.sessionMode.isLocked || Main.sessionMode.isGreeter;
        const presentationVisible = Boolean(this._snapshot?.visible);
        this._root.visible = presentationVisible && !Main.overview.visible && !sessionUnavailable;
    }

    _reposition() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor || !this._orb) return;
        this._root.set_position(0, 0);
        this._root.set_size(global.stage.width, global.stage.height);
        if (this._orb.x === 0 && this._orb.y === 0)
            this._orb.set_position(monitor.x + monitor.width - 110, monitor.y + 100);
        this._clampOrb();
    }

    _clampOrb() {
        const monitor = Main.layoutManager.findMonitorForActor(this._orb) ?? Main.layoutManager.primaryMonitor;
        if (!monitor) return;
        this._orb.set_position(
            Math.min(Math.max(this._orb.x, monitor.x + 16), monitor.x + monitor.width - this._orb.width - 16),
            Math.min(Math.max(this._orb.y, monitor.y + 16), monitor.y + monitor.height - this._orb.height - 16));
        this._positionPanel();
    }

    _positionPanel() {
        if (!this._panel || !this._orb) return;
        const placeLeft = this._snapshot?.panelPosition === 'left';
        const x = placeLeft ? this._orb.x - this._panel.width - 10 : this._orb.x + this._orb.width + 10;
        this._panel.set_position(x, this._orb.y);
    }
}
