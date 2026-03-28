from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path


class NoCacheRequestHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        base_dir = Path(__file__).parent
        super().__init__(*args, directory=str(base_dir), **kwargs)

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache, no-store, must-revalidate")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()


if __name__ == "__main__":
    server = HTTPServer(("0.0.0.0", 8000), NoCacheRequestHandler)
    print("Serving updater-test/ at http://localhost:8000/")
    server.serve_forever()