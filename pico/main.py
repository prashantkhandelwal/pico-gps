import network
import select
import socket
import time
from machine import UART, Pin


WIFI_SSID = "GHOST"
WIFI_PASSWORD = "supernova581"
HTTP_PORT = 80
BUFFER_SIZE = 8192

PAGE = b"""<!doctype html>
<html lang="en"><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Pico GPS</title>
<h1>Raw GPS data</h1>
<p id="status">Connecting...</p>
<pre id="data" style="white-space:pre-wrap;overflow-wrap:anywhere"></pre>
<script>
const output = document.getElementById("data");
const status = document.getElementById("status");
const decoder = new TextDecoder();
async function poll() {
  try {
    const response = await fetch("/data", {
      cache: "no-store", signal: AbortSignal.timeout(5000)
    });
    if (!response.ok) throw new Error("HTTP " + response.status);
    const bytes = await response.arrayBuffer();
    output.textContent = (output.textContent +
      decoder.decode(bytes, {stream: true})).slice(-20000);
    status.textContent = bytes.byteLength
      ? "Receiving GPS data" : "Connected; waiting for GPS data";
  } catch (error) {
    status.textContent = "Connection error: " + error.message + "; retrying...";
  } finally {
    setTimeout(poll, 500);
  }
}
poll();
</script></html>
"""


def connect_wifi(wlan):
    if wlan.isconnected():
        return

    wlan.disconnect()
    wlan.connect(WIFI_SSID, WIFI_PASSWORD)
    started = time.ticks_ms()
    while not wlan.isconnected():
        status = wlan.status()
        if status < 0:
            raise OSError("Wi-Fi connection failed, status: {}".format(status))
        if time.ticks_diff(time.ticks_ms(), started) >= 20000:
            raise OSError("Wi-Fi connection timed out")
        time.sleep_ms(250)


def send_all(client, data):
    remaining = memoryview(data)
    while remaining:
        sent = client.send(remaining)
        if not sent:
            raise OSError("HTTP client disconnected during send")
        remaining = remaining[sent:]


def respond(client, status, content_type, body):
    headers = (
        "HTTP/1.1 {}\r\n"
        "Content-Type: {}\r\n"
        "Content-Length: {}\r\n"
        "Cache-Control: no-store\r\n"
        "Connection: close\r\n\r\n"
    ).format(status, content_type, len(body)).encode()
    send_all(client, headers)
    send_all(client, body)


def handle_client(client, pending):
    client.settimeout(2)
    request = bytearray()
    started = time.ticks_ms()
    while b"\r\n\r\n" not in request:
        if time.ticks_diff(time.ticks_ms(), started) >= 2000:
            raise OSError("HTTP request timed out")
        chunk = client.recv(512)
        if not chunk:
            raise OSError("HTTP client disconnected during request")
        request.extend(chunk)
        if len(request) > 2048:
            print("HTTP request rejected: headers too large")
            respond(client, "431 Request Header Fields Too Large", "text/plain", b"Headers too large")
            return

    parts = bytes(request).split(b"\r\n", 1)[0].split()
    if len(parts) != 3 or parts[0] != b"GET":
        print("HTTP request rejected: expected GET request")
        respond(client, "400 Bad Request", "text/plain", b"Expected GET request")
    elif parts[1] == b"/":
        respond(client, "200 OK", "text/html; charset=utf-8", PAGE)
    elif parts[1] == b"/data":
        respond(client, "200 OK", "text/plain", bytes(pending))
        # Keep buffered data if sending raises an error.
        pending[:] = b""
    else:
        respond(client, "404 Not Found", "text/plain", b"Not found")


def collect_gps(uart, pending):
    data = uart.read(1024) if uart.any() else None
    if data:
        pending.extend(data)
        overflow = len(pending) - BUFFER_SIZE
        if overflow > 0:
            pending[:] = pending[overflow:]
            print("GPS buffer full; dropped", overflow, "oldest bytes")


def serve_gps(uart, wlan):
    server = socket.socket()
    try:
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("0.0.0.0", HTTP_PORT))
        server.listen(1)
        poller = select.poll()
        poller.register(server, select.POLLIN)
        pending = bytearray()
        print("Open http://{}:{}/ in your browser".format(wlan.ifconfig()[0], HTTP_PORT))
        while True:
            if not wlan.isconnected():
                raise OSError("Wi-Fi connection lost")
            collect_gps(uart, pending)
            if poller.poll(100):
                client, address = server.accept()
                try:
                    handle_client(client, pending)
                except OSError as exc:
                    print("HTTP client error:", address, exc)
                finally:
                    client.close()
    finally:
        server.close()


def main():
    if not WIFI_SSID:
        raise ValueError("Set WIFI_SSID and WIFI_PASSWORD before running")

    uart = UART(
        0, baudrate=9600, tx=Pin(0), rx=Pin(1),
        bits=8, parity=None, stop=1, rxbuf=8192,
    )
    wlan = network.WLAN(network.STA_IF)
    wlan.active(True)

    while True:
        try:
            connect_wifi(wlan)
            serve_gps(uart, wlan)
        except OSError as exc:
            print("GPS connection error:", exc, "- retrying in 5 seconds")
        time.sleep(5)


if __name__ == "__main__":
    main()

