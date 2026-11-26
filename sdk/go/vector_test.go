package velarouter

import (
	"crypto/ecdh"
	"encoding/base64"
	"encoding/hex"
	"testing"
)

// The known-answer vector from docs/PROTOCOL.md §3.4. If this test fails, the Go
// SDK will not interoperate with any other implementation.
func TestKnownAnswerVector(t *testing.T) {
	viewerScalar := scalar(1)
	agentScalar := scalar(2)

	viewerPriv, err := ecdh.P256().NewPrivateKey(viewerScalar)
	if err != nil {
		t.Fatal(err)
	}
	agentPriv, err := ecdh.P256().NewPrivateKey(agentScalar)
	if err != nil {
		t.Fatal(err)
	}

	const (
		wantViewerPub = "BGsX0fLhLEJH+Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT+NC4v4af5uO5+tKfA+eFivOM1drMV7Oy7ZAaDe/UfU="
		wantAgentPub  = "BHzyexiNA09+ilI4AwS1GsPAiWnid/IbNaYLSPxHZpl4B3dVENuO0EApPZrGn3Qw27p9reY86YIpngS3nSJ4c9E="
		wantShared    = "7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc47669978"
		wantKey       = "251d80ca3d587258fef045dad50e815efce55f634cad47f89a3a913be2b56238"
		wantAgentPrf  = "DgVzWu+hMaJiooLH+ErnGm8Kwqr52QHSHJLskvzikN0="
		wantViewerPrf = "q+S7nllk/myg+t5yobiRHo2NqwwFxcjaG3IkZgPuJVQ="
	)

	if got := base64.StdEncoding.EncodeToString(viewerPriv.PublicKey().Bytes()); got != wantViewerPub {
		t.Fatalf("viewer pub\n got %s\nwant %s", got, wantViewerPub)
	}
	if got := base64.StdEncoding.EncodeToString(agentPriv.PublicKey().Bytes()); got != wantAgentPub {
		t.Fatalf("agent pub\n got %s\nwant %s", got, wantAgentPub)
	}
	shared, err := viewerPriv.ECDH(agentPriv.PublicKey())
	if err != nil {
		t.Fatal(err)
	}
	if got := hex.EncodeToString(shared); got != wantShared {
		t.Fatalf("shared\n got %s\nwant %s", got, wantShared)
	}
	key, err := deriveSessionKey(shared, "s1", "123456")
	if err != nil {
		t.Fatal(err)
	}
	if got := hex.EncodeToString(key); got != wantKey {
		t.Fatalf("key\n got %s\nwant %s", got, wantKey)
	}
	if got := proof(key, "agent", "s1"); got != wantAgentPrf {
		t.Fatalf("agent proof\n got %s\nwant %s", got, wantAgentPrf)
	}
	if got := proof(key, "viewer", "s1"); got != wantViewerPrf {
		t.Fatalf("viewer proof\n got %s\nwant %s", got, wantViewerPrf)
	}
}

func TestMediaFrameParsing(t *testing.T) {
	payload := []byte{0xFF, 0xD8, 0xFF, 0xD9}
	frame := make([]byte, 24+len(payload))
	copy(frame, "VRF1")
	frame[4] = 1 // version
	frame[5] = 1 // JPEG
	frame[6] = 1 // keyframe
	frame[8] = 0
	frame[9] = 0
	frame[10] = 0
	frame[11] = 7 // frame_id = 7
	frame[16] = 0x06
	frame[17] = 0x40 // width = 1600
	frame[18] = 0x03
	frame[19] = 0x84 // height = 900
	frame[20] = 0
	frame[21] = 0
	frame[22] = 0
	frame[23] = byte(len(payload))
	copy(frame[24:], payload)

	got, err := parseFrame(frame)
	if err != nil {
		t.Fatal(err)
	}
	if got.ID != 7 || got.Width != 1600 || got.Height != 900 || got.Codec != 1 {
		t.Fatalf("unexpected header: %+v", got)
	}
	if string(got.Payload) != string(payload) {
		t.Fatalf("payload mismatch: %x", got.Payload)
	}
}

func TestMalformedFrameIsRejected(t *testing.T) {
	if _, err := parseFrame([]byte("VRF1")); err == nil {
		t.Fatal("short frame must fail")
	}
	bad := make([]byte, 24)
	copy(bad, "XXXX")
	if _, err := parseFrame(bad); err == nil {
		t.Fatal("bad magic must fail")
	}
}

func TestIVLayout(t *testing.T) {
	iv := makeIV(2, 0x0102030405060708)
	if iv[0] != 2 || iv[1] != 0 || iv[2] != 0 || iv[3] != 0 {
		t.Fatalf("direction prefix wrong: %x", iv[:4])
	}
	want := []byte{0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08}
	for i, b := range want {
		if iv[4+i] != b {
			t.Fatalf("counter bytes wrong: %x", iv[4:])
		}
	}
}

func scalar(n byte) []byte {
	out := make([]byte, 32)
	out[31] = n
	return out
}
