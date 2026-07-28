// src/App.tsx - Chunk 1: Draggable FAB & Toggleable Dropdown Container
// The entire 380x520 window is transparent. Only the circle and dropdown are visible.
// Dragging is handled by mousedown+move detection (separate from click).
import { useState, useRef, useCallback } from "react";
import { Mic, MicOff, Send, Sparkles, AudioWaveform, X } from "lucide-react";
import "./App.css";

interface Message {
  id: string;
  sender: "user" | "ai";
  text: string;
}

export default function App() {
  const [isOpen, setIsOpen] = useState(false);
  const [isListening, setIsListening] = useState(false);
  const [inputVal, setInputVal] = useState("");
  const [messages, setMessages] = useState<Message[]>([
    {
      id: "1",
      sender: "ai",
      text: "Hello! I'm ChatHead AI, your system-wide assistant. You can drag my circle anywhere, press Super+W to speak, or type below!",
    },
    {
      id: "2",
      sender: "user",
      text: "How do I check my system memory on Linux?",
    },
    {
      id: "3",
      sender: "ai",
      text: "You can run the command `free -h` or `btop` in your terminal to see real-time RAM usage!",
    },
  ]);

  // Track whether the mousedown was a drag or a click
  const isDragging = useRef(false);
  const mouseDownPos = useRef({ x: 0, y: 0 });

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    isDragging.current = false;
    mouseDownPos.current = { x: e.clientX, y: e.clientY };
  }, []);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    const dx = Math.abs(e.clientX - mouseDownPos.current.x);
    const dy = Math.abs(e.clientY - mouseDownPos.current.y);
    if (dx > 3 || dy > 3) {
      isDragging.current = true;
    }
  }, []);

  const handleMouseUp = useCallback(() => {
    // Only toggle dropdown if it was a click (not a drag)
    if (!isDragging.current) {
      setIsOpen((prev) => !prev);
    }
    isDragging.current = false;
  }, []);

  const toggleVoiceMode = (e: React.MouseEvent) => {
    e.stopPropagation();
    setIsListening(!isListening);
  };

  const handleSend = (e?: React.FormEvent) => {
    if (e) e.preventDefault();
    if (!inputVal.trim()) return;

    const newMsg: Message = {
      id: Date.now().toString(),
      sender: "user",
      text: inputVal,
    };

    setMessages((prev) => [...prev, newMsg]);
    setInputVal("");

    // Mock AI reply for Chunk 1 UI demo
    setTimeout(() => {
      setMessages((prev) => [
        ...prev,
        {
          id: (Date.now() + 1).toString(),
          sender: "ai",
          text: `Processing: "${newMsg.text}" via native CLI wrapper...`,
        },
      ]);
    }, 600);
  };

  return (
    <div className="app-container">
      {/* Circle Draggable FAB Button — positioned top-right of the transparent window */}
      <div className="fab-wrapper">
        <div
          className={`fab-button ${isListening ? "listening" : ""}`}
          data-tauri-drag-region
          onMouseDown={handleMouseDown}
          onMouseMove={handleMouseMove}
          onMouseUp={handleMouseUp}
          title="Click to Open/Close | Drag to Move"
        >
          {/* Animated Listening Pulse Ring */}
          {isListening && <div className="pulse-ring" />}

          {/* Icon state — pointerEvents:none so they don't block the drag region */}
          <div className="fab-icon" style={{ pointerEvents: "none" }}>
            {isOpen ? (
              <X size={26} color="#fff" />
            ) : isListening ? (
              <AudioWaveform size={26} color="#ec4899" />
            ) : (
              <Sparkles size={26} color="#8a5cf6" />
            )}
          </div>
        </div>
      </div>

      {/* Quake-Style Dropdown Container — only visible when isOpen */}
      {isOpen && (
        <div className="dropdown-card open">
          {/* Card Header */}
          <div className="card-header">
            <div className="header-title">
              <Sparkles size={16} color="#8a5cf6" />
              <span>ChatHead AI</span>
            </div>
            <div
              className="status-pill"
              style={{
                borderColor: isListening
                  ? "rgba(236, 72, 153, 0.4)"
                  : undefined,
                color: isListening ? "#ec4899" : undefined,
                background: isListening
                  ? "rgba(236, 72, 153, 0.15)"
                  : undefined,
              }}
            >
              <div
                className="status-dot"
                style={{
                  background: isListening ? "#ec4899" : undefined,
                }}
              />
              <span>
                {isListening ? "Listening (Super+W)..." : "CLI Ready"}
              </span>
            </div>
          </div>

          {/* Chat Messages Area */}
          <div className="chat-body">
            {messages.map((msg) => (
              <div key={msg.id} className={`chat-message ${msg.sender}`}>
                {msg.text.includes("`") ? (
                  <span>
                    {msg.text.split("`").map((part, idx) =>
                      idx % 2 === 1 ? <code key={idx}>{part}</code> : part
                    )}
                  </span>
                ) : (
                  msg.text
                )}
              </div>
            ))}
          </div>

          {/* Footer Input Area */}
          <form className="chat-footer" onSubmit={handleSend}>
            <div className="input-container">
              <input
                type="text"
                className="chat-input"
                placeholder={
                  isListening ? "Listening to voice..." : "Type prompt or speak..."
                }
                value={inputVal}
                onChange={(e) => setInputVal(e.target.value)}
              />

              {/* Mic Toggle Button */}
              <button
                type="button"
                className={`icon-btn ${isListening ? "active" : ""}`}
                onClick={toggleVoiceMode}
                title="Toggle Voice Mode"
              >
                {isListening ? <MicOff size={18} /> : <Mic size={18} />}
              </button>

              {/* Send Button */}
              <button
                type="submit"
                className="icon-btn send-btn"
                title="Send Prompt"
              >
                <Send size={16} />
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
