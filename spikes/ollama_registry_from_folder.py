# Spike: serve an Ollama-compatible registry purely from a local content-addressed dir.
import http.server, sys, os, re
CAS=sys.argv[1]; MAN=sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    protocol_version="HTTP/1.1"
    def log_message(self,*a): pass
    def do(self):
        sys.stderr.write(f"{self.command} {self.path} | UA={self.headers.get('User-Agent')} | Range={self.headers.get('Range')}\n"); sys.stderr.flush()
        p=self.path
        if "/manifests/" in p:
            b=open(MAN,'rb').read(); self.send_response(200); self.send_header("Content-Type","application/vnd.docker.distribution.manifest.v2+json"); self.send_header("Content-Length",str(len(b))); self.end_headers()
            if self.command!="HEAD": self.wfile.write(b)
            return
        m=re.search(r"sha256:([0-9a-f]{64})",p)
        if not m: self.send_response(404); self.send_header("Content-Length","0"); self.end_headers(); return
        f=os.path.join(CAS,m.group(1)); size=os.path.getsize(f)
        if p.startswith("/v2/") and self.command=="GET" and not self.headers.get("Range"):
            self.send_response(307); self.send_header("Location",f"http://127.0.0.1:8099/cas/sha256:{m.group(1)}"); self.send_header("Content-Length","0"); self.end_headers(); return
        rng=self.headers.get("Range"); start,end=0,size-1
        if rng:
            a,b=re.match(r"bytes=(\d*)-(\d*)",rng).groups(); start=int(a) if a else 0; end=int(b) if b else size-1
            self.send_response(206); self.send_header("Content-Range",f"bytes {start}-{end}/{size}")
        else: self.send_response(200)
        self.send_header("Content-Length",str(end-start+1)); self.send_header("Accept-Ranges","bytes"); self.send_header("Docker-Content-Digest","sha256:"+m.group(1)); self.end_headers()
        if self.command=="HEAD": return
        with open(f,'rb') as fh:
            fh.seek(start); left=end-start+1
            while left>0:
                b=fh.read(min(1<<20,left)); self.wfile.write(b); left-=len(b)
    do_GET=do_HEAD=do
http.server.ThreadingHTTPServer(("127.0.0.1",8099),H).serve_forever()
