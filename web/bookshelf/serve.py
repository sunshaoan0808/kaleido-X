#!/usr/bin/env python3
"""Simple HTTP server: serves static files + proxies /api/ to backend"""
import http.server
import urllib.request
import os
import sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8088
BACKEND = 'http://127.0.0.1:18766'
STATIC_DIR = '${REPO:-.}/web/bookshelf'

class ProxyHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=STATIC_DIR, **kwargs)
    
    def translate_path(self, path):
        # Strip /web/bookshelf/ prefix for local dev
        if path.startswith('/web/bookshelf/'):
            path = path[len('/web/bookshelf/'):]
        elif path == '/web/bookshelf' or path == '/web/bookshelf':
            path = '/'
        return super().translate_path(path)
    
    def do_GET(self):
        # OPDS/URL proxy: fetch arbitrary URL to bypass CORS
        if self.path.startswith('/opds-proxy'):
            from urllib.parse import urlparse, parse_qs, urljoin
            qs = parse_qs(urlparse(self.path).query)
            target = qs.get('url', [None])[0]
            if not target or not target.startswith(('http://', 'https://')):
                self.send_response(400)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(b'{"error":"missing or invalid url"}')
                return
            try:
                req = urllib.request.Request(target, headers={'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) Kaleido-Bookshelf/1.0', 'Accept': '*/*'})
                host = urlparse(target).netloc
                if host in ('localhost', '127.0.0.1', '::1'):
                    # 本地目标必须直连（系统代理会拒绝 localhost）
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                    with opener.open(req, timeout=30) as resp:
                        data = resp.read()
                        self.send_response(resp.status)
                        ct = resp.headers.get('Content-Type', 'application/octet-stream')
                        self.send_header('Content-Type', ct)
                        self.send_header('Access-Control-Allow-Origin', '*')
                        self.send_header('Content-Length', str(len(data)))
                        self.end_headers()
                        self.wfile.write(data)
                    return
                with urllib.request.urlopen(req, timeout=30) as resp:
                    data = resp.read()
                    self.send_response(resp.status)
                    ct = resp.headers.get('Content-Type', 'application/octet-stream')
                    self.send_header('Content-Type', ct)
                    self.send_header('Access-Control-Allow-Origin', '*')
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
            except urllib.error.HTTPError as e:
                self.send_response(e.code)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Access-Control-Allow-Origin', '*')
                self.end_headers()
                self.wfile.write(f'{{"error":"http {e.code}","body":{e.read()[:200]!r}}}'.encode())
            except Exception as e:
                self.send_response(502)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Access-Control-Allow-Origin', '*')
                self.end_headers()
                self.wfile.write(f'{{"error":"{e}"}}'.encode())
            return
        # Handle API proxy
        if self.path.startswith('/api/'):
            url = BACKEND + self.path
            try:
                req = urllib.request.Request(url)
                with urllib.request.urlopen(req) as resp:
                    data = resp.read()
                    self.send_response(resp.status)
                    for k, v in resp.headers.items():
                        if k.lower() not in ('transfer-encoding', 'content-encoding', 'content-length', 'connection'):
                            self.send_header(k, v)
                    self.send_header('Access-Control-Allow-Origin', '*')
                    self.end_headers()
                    self.wfile.write(data)
            except urllib.error.HTTPError as e:
                self.send_response(e.code)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(e.read())
            except Exception as e:
                self.send_error(502, f'Proxy error: {e}')
        else:
            super().do_GET()
    
    def do_OPTIONS(self):
        self.send_response(200)
        self.send_header('Access-Control-Allow-Origin', '*')
        self.send_header('Access-Control-Allow-Methods', 'GET, POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', '*')
        self.end_headers()
    
    def log_message(self, format, *args):
        pass  # suppress logs

if __name__ == '__main__':
    server = http.server.ThreadingHTTPServer(('0.0.0.0', PORT), ProxyHandler)
    print(f'Serving bookshelf on http://0.0.0.0:{PORT} (API proxy -> {BACKEND})')
    sys.stdout.flush()
    server.serve_forever()
