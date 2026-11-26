// Command smoke connects to a Velarouter agent, prints the agent capabilities and
// saves one JPEG frame. It is the smallest possible end-to-end check for the Go
// SDK, and the starting point for a MyGo client.
//
//	go run ./cmd/smoke -url ws://127.0.0.1:34789/vr -code 654321 -out shot.jpg
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"

	velarouter "github.com/logdns/Velarouter/sdk/go"
)

func main() {
	url := flag.String("url", "ws://127.0.0.1:34789/vr", "agent websocket url")
	code := flag.String("code", "", "pairing code")
	out := flag.String("out", "frame.jpg", "where to write the first frame")
	timeout := flag.Duration("timeout", 20*time.Second, "overall timeout")
	flag.Parse()

	if *code == "" {
		fmt.Fprintln(os.Stderr, "-code is required")
		os.Exit(2)
	}

	ctx, cancel := context.WithTimeout(context.Background(), *timeout)
	defer cancel()

	start := time.Now()
	client, err := velarouter.Dial(ctx, *url, *code, velarouter.Options{App: "velarouter-go-smoke/0.1"})
	if err != nil {
		fmt.Fprintf(os.Stderr, "dial: %v\n", err)
		os.Exit(1)
	}
	defer client.Close()
	peer := client.Peer()
	fmt.Printf("handshake ok in %v (device=%v)\n", time.Since(start).Round(time.Millisecond), peer.Device["name"])

	if err := client.RequestVideo(1600, 900, 60, 30); err != nil {
		fmt.Fprintf(os.Stderr, "request video: %v\n", err)
		os.Exit(1)
	}

	var frames int
	var bytes int
	for frames < 3 {
		frame, err := client.Next(ctx, func(c velarouter.Control) {
			if s, ok := velarouter.StatsFrom(c); ok {
				fmt.Printf("  stats: %.1f fps, %d kbps, rtt %d ms\n", s.FPS, s.Kbps, s.RTTms)
			}
		})
		if err != nil {
			fmt.Fprintf(os.Stderr, "next: %v\n", err)
			os.Exit(1)
		}
		frames++
		bytes += len(frame.Payload)
		fmt.Printf("frame %d: %dx%d %d bytes (codec %d)\n",
			frame.ID, frame.Width, frame.Height, len(frame.Payload), frame.Codec)
		if frames == 1 {
			if err := os.WriteFile(*out, frame.Payload, 0o644); err != nil {
				fmt.Fprintf(os.Stderr, "write: %v\n", err)
				os.Exit(1)
			}
			fmt.Printf("saved %s\n", *out)
		}
		// Request the next frame explicitly; a real client would pipeline input.
		if err := client.MouseMove(32768, 16384, 0); err != nil {
			fmt.Fprintf(os.Stderr, "input: %v\n", err)
		}
	}
	fmt.Printf("ok: %d frames, %.1f KiB\n", frames, float64(bytes)/1024)
}
