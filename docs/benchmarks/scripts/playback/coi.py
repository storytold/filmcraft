import http.server,sys,functools
class H(http.server.SimpleHTTPRequestHandler):
    def end_headers(s):
        s.send_header('Cross-Origin-Opener-Policy','same-origin');s.send_header('Cross-Origin-Embedder-Policy','require-corp');super().end_headers()
    def log_message(s,*a):pass
http.server.ThreadingHTTPServer(('127.0.0.1',int(sys.argv[1])),functools.partial(H,directory=sys.argv[2])).serve_forever()
