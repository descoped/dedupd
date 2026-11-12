# Spike: an HF_ENDPOINT server that hides Xet, so that huggingface_hub falls back to plain HTTP through it.
import http.server, http.client, sys, json, urllib.parse, re
def up(method, url, headers, body=None):
    u=urllib.parse.urlsplit(url); c=http.client.HTTPSConnection(u.netloc)
    c.request(method, u.path+("?"+u.query if u.query else ""), body=body, headers=headers); return c.getresponse()
class H(http.server.BaseHTTPRequestHandler):
    protocol_version="HTTP/1.1"
    def log_message(self,*a): pass
    def do(self):
        n=int(self.headers.get("Content-Length") or 0); body=self.rfile.read(n) if n else None
        hdrs={k:v for k,v in self.headers.items() if k.lower() not in("host","connection","accept-encoding")}
        r=up(self.command,"https://huggingface.co"+self.path,hdrs,body)
        meta={k.lower():v for k,v in r.getheaders()}
        note=""
        if "/resolve/" in self.path and r.status in (301,302,307,308) and meta.get("location","").startswith("http"):
            # LFS/Xet file: answer ourselves, never expose the CDN or Xet hints
            size=meta["x-linked-size"]; etag=meta["x-linked-etag"]; r.read()
            if self.command=="HEAD":
                self.send_response(200); out={"X-Repo-Commit":meta["x-repo-commit"],"ETag":etag,"Content-Length":size,"Accept-Ranges":"bytes"}
                for k,v in out.items(): self.send_header(k,v)
                self.end_headers(); note="HEAD answered locally"
            else:
                h2={}; 
                if self.headers.get("Range"): h2["Range"]=self.headers["Range"]
                r2=up("GET",meta["location"],h2)
                self.send_response(r2.status)
                for k in ("Content-Length","Content-Range","Accept-Ranges"):
                    if r2.getheader(k): self.send_header(k,r2.getheader(k))
                self.send_header("ETag",etag); self.send_header("X-Repo-Commit",meta["x-repo-commit"]); self.end_headers()
                tot=0
                while True:
                    b=r2.read(1<<20)
                    if not b: break
                    self.wfile.write(b); tot+=len(b)
                note=f"GET streamed {tot} bytes through this endpoint"
        else:
            data=r.read() if self.command!="HEAD" else b""
            if ("/tree/" in self.path or "/paths-info/" in self.path) and r.status==200:
                j=json.loads(data); k=sum(1 for e in j if e.pop("xetHash",None)); data=json.dumps(j).encode(); note=f"stripped xetHash from {k} entries"
            self.send_response(r.status)
            for k,v in r.getheaders():
                kl=k.lower()
                if kl in("transfer-encoding","connection","content-length","content-encoding","strict-transport-security","alt-svc","x-xet-hash","x-xet-refresh-route"): continue
                if kl=="link":
                    v=", ".join(p for p in v.split(", ") if "xet" not in p).replace("https://huggingface.co","http://127.0.0.1:8098")
                    if not v: continue
                self.send_header(k,v)
            self.send_header("Content-Length", meta.get("content-length","0") if self.command=="HEAD" else str(len(data))); self.end_headers(); self.wfile.write(data)
        sys.stderr.write(f"{self.command} {r.status} {self.path[:110]} range={self.headers.get('Range')} {note}\n"); sys.stderr.flush()
    do_GET=do_HEAD=do_POST=do
http.server.ThreadingHTTPServer(("127.0.0.1",8098),H).serve_forever()
