#!/usr/bin/env python3
"""
D2L / IIS WebDAV simulation proxy for SCS-RClient E2E tests.
Wraps an upstream Copyparty server and reproduces Desire2Learn / IIS WebDAV characteristics:
1. Rejects PROPFIND requests with 'Depth: infinity' by returning HTTP 403 Forbidden (Content-Length: 0).
2. Rewrites PROPFIND XML responses to wrap <d:href> in <![CDATA[...]]> with the 'd:' namespace prefix.
3. Forwards all other requests (GET, PUT, MKCOL, DELETE, OPTIONS, etc.) transparently.
"""
import http.server
import re
import sys
import urllib.error
import urllib.request

UPSTREAM_PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 3924
LISTEN_PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 3923


class D2LWebdavMockProxy(http.server.BaseHTTPRequestHandler):
    def do_PROPFIND(self):
        depth = self.headers.get("Depth", "").strip().lower()
        if depth == "infinity":
            # Desire2Learn / IIS WebDAV rejects Depth: infinity with HTTP 403 Forbidden
            self.send_response(403, "Forbidden")
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "keep-alive")
            self.end_headers()
            return
        self.proxy_request()

    def do_GET(self):
        self.proxy_request()

    def do_PUT(self):
        self.proxy_request()

    def do_DELETE(self):
        self.proxy_request()

    def do_MKCOL(self):
        self.proxy_request()

    def do_OPTIONS(self):
        self.proxy_request()

    def do_HEAD(self):
        self.proxy_request()

    def proxy_request(self):
        url = f"http://127.0.0.1:{UPSTREAM_PORT}{self.path}"
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length > 0 else None

        headers = {}
        for k, v in self.headers.items():
            if k.lower() not in ("host", "content-length"):
                headers[k] = v

        req = urllib.request.Request(
            url, data=body, headers=headers, method=self.command
        )
        try:
            with urllib.request.urlopen(req) as resp:
                status = resp.status
                resp_headers = resp.headers
                content = resp.read()
        except urllib.error.HTTPError as e:
            status = e.code
            resp_headers = e.headers
            content = e.read()
        except Exception as e:
            self.send_response(502, "Bad Gateway")
            self.end_headers()
            self.wfile.write(str(e).encode())
            return

        # If PROPFIND 207 Multi-Status, rewrite XML to match D2L Brightspace CDATA format
        if self.command == "PROPFIND" and status == 207:
            xml_text = content.decode("utf-8", errors="replace")

            # Wrap href in <![CDATA[...]]> like D2L
            def wrap_cdata(m):
                val = m.group(1).strip()
                if val.startswith("<![CDATA["):
                    return f"<d:href>{val}</d:href>"
                return f"<d:href><![CDATA[{val}]]></d:href>"

            xml_text = re.sub(
                r"<(?:\w+:)?href>(.*?)</(?:\w+:)?href>",
                wrap_cdata,
                xml_text,
                flags=re.DOTALL,
            )
            # Use 'd:' namespace prefix as D2L Brightspace does
            xml_text = re.sub(r"<D:", "<d:", xml_text)
            xml_text = re.sub(r"</D:", "</d:", xml_text)
            xml_text = xml_text.replace('xmlns:D="DAV:"', 'xmlns:d="DAV:"')
            content = xml_text.encode("utf-8")

        self.send_response(status)
        for k, v in resp_headers.items():
            if k.lower() not in ("content-length", "transfer-encoding"):
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, format, *args):
        # Keep test output clean
        pass


if __name__ == "__main__":
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", LISTEN_PORT), D2LWebdavMockProxy
    )
    print(
        f"D2L mock proxy listening on port {LISTEN_PORT} -> Copyparty upstream port {UPSTREAM_PORT}"
    )
    server.serve_forever()
