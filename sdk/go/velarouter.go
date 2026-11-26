// Package velarouter is a dependency-light Go client for the VRP/1 protocol.
//
// It exists for two reasons:
//
//  1. MyGo (https://mygo.run) clients are Go programs, so they need a Go client.
//  2. A third independent implementation is the cheapest way to prove the
//     protocol is actually implementable from the spec.
//
// The API is intentionally small: Dial → Next → Input → Close.
package velarouter

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/ecdh"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/gorilla/websocket"
	"golang.org/x/crypto/hkdf"
)

const (
	protocolVersion = 1
	envelopeControl = 1
	envelopeMedia   = 2
	hkdfInfo        = "velarouter/vrp1/session-key"
	aad             = "vrp1"
)

// Capabilities mirrors vr_proto::Capabilities.
type Capabilities struct {
	Codecs       []string `json:"codecs"`
	MaxWidth     uint16   `json:"max_width"`
	MaxHeight    uint16   `json:"max_height"`
	MaxFPS       uint8    `json:"max_fps"`
	Input        bool     `json:"input"`
	Clipboard    bool     `json:"clipboard"`
	MultiDisplay bool     `json:"multi_display"`
	Audio        bool     `json:"audio"`
}

// DefaultCapabilities is what a JPEG-only viewer should advertise.
func DefaultCapabilities() Capabilities {
	return Capabilities{
		Codecs:       []string{"jpeg"},
		MaxWidth:     2560,
		MaxHeight:    1600,
		MaxFPS:       60,
		Input:        true,
		Clipboard:    true,
		MultiDisplay: true,
	}
}

// Frame is one decoded video frame.
type Frame struct {
	ID      uint32
	Display uint32
	Width   uint16
	Height  uint16
	Codec   uint8 // 1=JPEG 2=H.264 3=H.265 4=VP8 5=AV1
	Payload []byte
}

// InputEvent is the discriminated union used by Control.Input.
type InputEvent struct {
	Kind      string      `json:"k"`
	X         *uint16     `json:"x,omitempty"`
	Y         *uint16     `json:"y,omitempty"`
	Display   *uint32     `json:"display,omitempty"`
	Button    string      `json:"button,omitempty"`
	Down      *bool       `json:"down,omitempty"`
	DX        *int32      `json:"dx,omitempty"`
	DY        *int32      `json:"dy,omitempty"`
	Code      string      `json:"code,omitempty"`
	Modifiers *uint8      `json:"modifiers,omitempty"`
	Text      string      `json:"text,omitempty"`
	Phase     string      `json:"phase,omitempty"`
	Points    []TouchPoint `json:"points,omitempty"`
}

// TouchPoint is one finger in normalised coordinates.
type TouchPoint struct {
	ID uint16 `json:"id"`
	X  uint16 `json:"x"`
	Y  uint16 `json:"y"`
}

// Stats mirrors vr_proto::Stats.
type Stats struct {
	FPS       float32 `json:"fps"`
	Kbps      uint32  `json:"kbps"`
	RTTms     uint32  `json:"rtt_ms"`
	CaptureMs float32 `json:"capture_ms"`
	EncodeMs  float32 `json:"encode_ms"`
	DecodeMs  float32 `json:"decode_ms"`
	Dropped   uint32  `json:"dropped"`
	E2Ems     uint32  `json:"e2e_ms"`
}

// Control is a partially typed control message; unknown fields are preserved in
// Raw so callers can implement newer message types without a new SDK release.
type Control struct {
	Type string          `json:"t"`
	Raw  json.RawMessage `json:"-"`
}

// Options tunes a client.
type Options struct {
	App          string
	Capabilities Capabilities
	// HandshakeTimeout defaults to 10s.
	HandshakeTimeout time.Duration
}

// Client is one encrypted VRP session (viewer side).
type Client struct {
	conn  *websocket.Conn
	aead  cipher.AEAD
	tx    uint64
	rx    uint64
	sid   string
	code  string
	app   string
	caps  Capabilities
	peers PeerInfo
}

// PeerInfo is what the handshake learned about the agent.
type PeerInfo struct {
	App         string
	Device      map[string]any
	Capabilities Capabilities
}

// Peer returns the agent information discovered during the handshake.
func (c *Client) Peer() PeerInfo { return c.peers }

// Dial connects, performs the VRP/1 handshake and returns a ready session.
func Dial(ctx context.Context, url, pairingCode string, opts Options) (*Client, error) {
	if opts.App == "" {
		opts.App = "velarouter-go/0.1"
	}
	if opts.HandshakeTimeout == 0 {
		opts.HandshakeTimeout = 10 * time.Second
	}
	if len(opts.Capabilities.Codecs) == 0 {
		opts.Capabilities = DefaultCapabilities()
	}

	sid, err := randomHex(8)
	if err != nil {
		return nil, err
	}

	dialer := websocket.Dialer{HandshakeTimeout: opts.HandshakeTimeout}
	withSID := url
	if !hasQuery(withSID, "sid") {
		withSID = url + "?sid=" + sid
	}
	conn, _, err := dialer.DialContext(ctx, withSID, nil)
	if err != nil {
		return nil, fmt.Errorf("dial %s: %w", withSID, err)
	}
	c := &Client{conn: conn, sid: sid, code: pairingCode, app: opts.App, caps: opts.Capabilities}

	if err := c.handshake(ctx, opts.HandshakeTimeout); err != nil {
		_ = conn.Close()
		return nil, err
	}
	return c, nil
}

func hasQuery(url, key string) bool {
	for i := 0; i+len(key) < len(url); i++ {
		if url[i] == '?' || url[i] == '&' {
			if len(url) > i+1+len(key) && url[i+1:i+1+len(key)] == key && url[i+1+len(key)] == '=' {
				return true
			}
		}
	}
	return false
}

func (c *Client) handshake(ctx context.Context, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	_ = c.conn.SetReadDeadline(deadline)

	if err := c.writeJSON(map[string]any{
		"t": "hello", "version": protocolVersion, "role": "viewer",
		"session_id": c.sid, "app": c.app,
	}); err != nil {
		return err
	}

	priv, err := ecdh.P256().GenerateKey(rand.Reader)
	if err != nil {
		return err
	}
	pub := priv.PublicKey().Bytes() // uncompressed SEC1, 65 bytes
	nonce := make([]byte, 16)
	if _, err := rand.Read(nonce); err != nil {
		return err
	}
	if err := c.writeJSON(map[string]any{
		"t": "auth_init",
		"viewer_pub": base64.StdEncoding.EncodeToString(pub),
		"nonce":      base64.StdEncoding.EncodeToString(nonce),
	}); err != nil {
		return err
	}

	finish, err := c.readJSON(ctx)
	if err != nil {
		return err
	}
	if t, _ := finish["t"].(string); t == "auth_fail" {
		return fmt.Errorf("agent rejected: %v", finish["reason"])
	}
	if t, _ := finish["t"].(string); t != "auth_finish" {
		return fmt.Errorf("expected auth_finish, got %v", finish["t"])
	}
	agentPubB64, _ := finish["agent_pub"].(string)
	agentProof, _ := finish["proof"].(string)
	agentPubRaw, err := base64.StdEncoding.DecodeString(agentPubB64)
	if err != nil {
		return fmt.Errorf("agent_pub: %w", err)
	}
	agentPub, err := ecdh.P256().NewPublicKey(agentPubRaw)
	if err != nil {
		return fmt.Errorf("agent_pub: %w", err)
	}
	shared, err := priv.ECDH(agentPub)
	if err != nil {
		return err
	}
	key, err := deriveSessionKey(shared, c.sid, c.code)
	if err != nil {
		return err
	}
	if !hmac.Equal([]byte(agentProof), []byte(proof(key, "agent", c.sid))) {
		_ = c.writeJSON(map[string]any{"t": "auth_fail", "reason": "agent proof mismatch"})
		return errors.New("agent proof mismatch (wrong pairing code?)")
	}
	if err := c.writeJSON(map[string]any{
		"t": "auth_proof", "proof": proof(key, "viewer", c.sid),
	}); err != nil {
		return err
	}
	ok, err := c.readJSON(ctx)
	if err != nil {
		return err
	}
	if t, _ := ok["t"].(string); t != "auth_ok" {
		return fmt.Errorf("expected auth_ok, got %v (%v)", ok["t"], ok["reason"])
	}

	block, err := aes.NewCipher(key)
	if err != nil {
		return err
	}
	c.aead, err = cipher.NewGCM(block)
	if err != nil {
		return err
	}

	// First encrypted message in both directions is `caps`.
	if err := c.sendControl(map[string]any{"t": "caps", "capabilities": c.caps}); err != nil {
		return err
	}
	for {
		kind, payload, err := c.readSealed(ctx)
		if err != nil {
			return err
		}
		if kind != envelopeControl {
			continue
		}
		var msg map[string]any
		if err := json.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if t, _ := msg["t"].(string); t == "caps" {
			if raw, ok := msg["capabilities"]; ok {
				b, _ := json.Marshal(raw)
				_ = json.Unmarshal(b, &c.peers.Capabilities)
			}
			if dev, ok := msg["device"].(map[string]any); ok {
				c.peers.Device = dev
			}
			break
		}
	}
	_ = c.conn.SetReadDeadline(time.Time{})
	return nil
}

// RequestVideo asks the agent for a specific stream configuration.
func (c *Client) RequestVideo(width, height uint16, quality, fps uint8) error {
	if quality == 0 {
		quality = 70
	}
	if fps == 0 {
		fps = 30
	}
	return c.sendControl(map[string]any{
		"t": "video_config", "codec": "jpeg", "quality": quality, "fps": fps,
		"max_width": width, "max_height": height, "display": 0,
	})
}

// Next blocks until a media frame arrives. Control messages are delivered to
// onControl when it is non-nil.
func (c *Client) Next(ctx context.Context, onControl func(Control)) (*Frame, error) {
	for {
		type result struct {
			kind    byte
			payload []byte
			err     error
		}
		ch := make(chan result, 1)
		go func() {
			kind, payload, err := c.readSealed(ctx)
			ch <- result{kind, payload, err}
		}()
		select {
		case <-ctx.Done():
			return nil, ctx.Err()
		case r := <-ch:
			if r.err != nil {
				return nil, r.err
			}
			if r.kind == envelopeMedia {
				return parseFrame(r.payload)
			}
			var msg map[string]any
			if err := json.Unmarshal(r.payload, &msg); err != nil {
				continue
			}
			t, _ := msg["t"].(string)
			if onControl != nil {
				onControl(Control{Type: t, Raw: r.payload})
			}
			if t == "bye" {
				return nil, errors.New("session closed by agent")
			}
			if t == "ping" {
				_ = c.sendControl(map[string]any{"t": "pong", "ts": msg["ts"]})
			}
		}
	}
}

// Stats decodes a stats control message.
func StatsFrom(control Control) (Stats, bool) {
	var s Stats
	if control.Type != "stats" {
		return s, false
	}
	return s, json.Unmarshal(control.Raw, &s) == nil
}

// Input sends one input event.
func (c *Client) Input(event InputEvent) error {
	return c.sendControl(map[string]any{"t": "input", "event": event})
}

// MouseMove sends a normalised pointer position.
func (c *Client) MouseMove(x, y uint16, display uint32) error {
	return c.Input(InputEvent{Kind: "mouse_move", X: &x, Y: &y, Display: &display})
}

// Key sends a key press or release using a W3C KeyboardEvent.code value.
func (c *Client) Key(code string, down bool, modifiers uint8) error {
	return c.Input(InputEvent{Kind: "key", Code: code, Down: &down, Modifiers: &modifiers})
}

// Clipboard sends text to the agent.
func (c *Client) Clipboard(text string) error {
	return c.sendControl(map[string]any{"t": "clipboard", "direction": "viewer_to_agent", "text": text})
}

// Close sends a polite goodbye and closes the socket.
func (c *Client) Close() error {
	_ = c.sendControl(map[string]any{"t": "bye", "reason": "client closed"})
	return c.conn.Close()
}

// --- wire ------------------------------------------------------------------

func (c *Client) sendControl(v any) error {
	body, err := json.Marshal(v)
	if err != nil {
		return err
	}
	plain := make([]byte, 0, len(body)+1)
	plain = append(plain, envelopeControl)
	plain = append(plain, body...)
	return c.sendSealed(plain)
}

func (c *Client) sendSealed(plain []byte) error {
	iv := makeIV(1, c.tx)
	c.tx++
	sealed := c.aead.Seal(nil, iv, plain, []byte(aad))
	wire := make([]byte, 8+len(sealed))
	binary.BigEndian.PutUint64(wire[:8], c.tx-1)
	copy(wire[8:], sealed)
	return c.conn.WriteMessage(websocket.BinaryMessage, wire)
}

func (c *Client) readSealed(ctx context.Context) (byte, []byte, error) {
	_ = ctx
	mt, wire, err := c.conn.ReadMessage()
	if err != nil {
		return 0, nil, err
	}
	if mt != websocket.BinaryMessage || len(wire) < 8+c.aead.Overhead() {
		return 0, nil, fmt.Errorf("unexpected frame (type=%d, len=%d)", mt, len(wire))
	}
	counter := binary.BigEndian.Uint64(wire[:8])
	if counter != c.rx {
		return 0, nil, fmt.Errorf("counter gap: want %d got %d", c.rx, counter)
	}
	plain, err := c.aead.Open(nil, makeIV(2, c.rx), wire[8:], []byte(aad))
	if err != nil {
		return 0, nil, fmt.Errorf("decrypt: %w", err)
	}
	c.rx++
	if len(plain) == 0 {
		return 0, nil, errors.New("empty envelope")
	}
	return plain[0], plain[1:], nil
}

func (c *Client) writeJSON(v any) error {
	body, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return c.conn.WriteMessage(websocket.TextMessage, body)
}

func (c *Client) readJSON(ctx context.Context) (map[string]any, error) {
	_ = ctx
	for {
		mt, data, err := c.conn.ReadMessage()
		if err != nil {
			return nil, err
		}
		if mt != websocket.TextMessage {
			continue
		}
		var out map[string]any
		if err := json.Unmarshal(data, &out); err != nil {
			return nil, err
		}
		return out, nil
	}
}

func parseFrame(buf []byte) (*Frame, error) {
	const headerLen = 24
	if len(buf) < headerLen {
		return nil, errors.New("media frame too short")
	}
	if string(buf[:4]) != "VRF1" {
		return nil, errors.New("bad media magic")
	}
	length := binary.BigEndian.Uint32(buf[20:24])
	if int(length) > len(buf)-headerLen {
		return nil, errors.New("truncated media payload")
	}
	return &Frame{
		Codec:   buf[5],
		ID:      binary.BigEndian.Uint32(buf[8:12]),
		Display: binary.BigEndian.Uint32(buf[12:16]),
		Width:   binary.BigEndian.Uint16(buf[16:18]),
		Height:  binary.BigEndian.Uint16(buf[18:20]),
		Payload: buf[headerLen : headerLen+int(length)],
	}, nil
}

func makeIV(direction byte, counter uint64) []byte {
	iv := make([]byte, 12)
	iv[0] = direction
	binary.BigEndian.PutUint64(iv[4:], counter)
	return iv
}

func deriveSessionKey(shared []byte, sessionID, code string) ([]byte, error) {
	salt := []byte(sessionID + "|" + code)
	r := hkdf.New(sha256.New, shared, salt, []byte(hkdfInfo))
	key := make([]byte, 32)
	if _, err := r.Read(key); err != nil {
		return nil, err
	}
	return key, nil
}

func proof(key []byte, label, sessionID string) string {
	mac := hmac.New(sha256.New, key)
	mac.Write([]byte(label + "|" + sessionID))
	return base64.StdEncoding.EncodeToString(mac.Sum(nil))
}

func randomHex(n int) (string, error) {
	buf := make([]byte, n)
	if _, err := rand.Read(buf); err != nil {
		return "", err
	}
	return fmt.Sprintf("%x", buf), nil
}
