# Spike: logging pass-through for huggingface_hub clients (HF_ENDPOINT=http://127.0.0.1:8098).
# Forwards everything to huggingface.co WITHOUT following redirects, and logs request + key response headers.
import http.server, http.client, sys, json
class H(http.server.BaseHTTPRequestHandler):
    protocol_version="HTTP/1.1"
    def log_message(self,*a): pass
    def do(self):
        n=int(self.headers.get("Content-Length") or 0); body=self.rfile.read(n) if n else None
        hdrs={k:v for k,v in self.headers.items() if k.lower() not in("host","connection","accept-encoding")}
        c=http.client.HTTPSConnection("huggingface.co"); c.request(self.command,self.path,body=body,headers=hdrs); r=c.getresponse()
        keep={k:v[:110] for k,v in r.getheaders() if k.lower() in("location","etag","x-linked-etag","x-linked-size","x-repo-commit","x-xet-hash","link","content-length","x-error-code","accept-ranges","content-range")}
        sys.stderr.write(json.dumps({"m":self.command,"p":self.path[:140],"ua":(self.headers.get("User-Agent") or "")[:90],"range":self.headers.get("Range"),"auth":bool(self.headers.get("Authorization")),"status":r.status,"resp":keep})+"\n"); sys.stderr.flush()
        data=r.read() if self.command!="HEAD" else b""
        self.send_response(r.status)
        for k,v in r.getheaders():
            if k.lower() in("transfer-encoding","connection","content-length","content-encoding","strict-transport-security","alt-svc"): continue
            self.send_header(k,v)
        self.send_header("Content-Length", r.getheader("Content-Length") if self.command=="HEAD" and r.getheader("Content-Length") else str(len(data))); self.end_headers(); self.wfile.write(data)
    do_GET=do_HEAD=do_POST=do
http.server.ThreadingHTTPServer(("127.0.0.1",8098),H).serve_forever()
