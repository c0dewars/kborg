#!/usr/bin/env python3
"""Real offline Sigstore fixtures using upstream sigstore-go's virtual services.

No identity login, public-service signing, personal keyring or persistent private
key is used. Only public bundles/roots/keys/policies leave the temporary process.
A local Go compiler is fixture tooling; KBorg itself needs no Go runtime.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

SOURCE = r'''
package main
import (
 "bytes"
 "context"
 "crypto"
 "crypto/ecdsa"
 "crypto/elliptic"
 "crypto/rand"
 "crypto/sha256"
 "crypto/x509"
 "encoding/base64"
 "encoding/hex"
 "encoding/json"
 "encoding/pem"
 "fmt"
 "os"
 "path/filepath"
 "time"
 pb "github.com/sigstore/protobuf-specs/gen/pb-go/bundle/v1"
 common "github.com/sigstore/protobuf-specs/gen/pb-go/common/v1"
 rekor "github.com/sigstore/protobuf-specs/gen/pb-go/rekor/v1"
 "github.com/sigstore/sigstore-go/pkg/root"
 "github.com/sigstore/sigstore-go/pkg/testing/ca"
 "github.com/sigstore/sigstore-go/pkg/tlog"
 "github.com/sigstore/sigstore/pkg/signature"
 "github.com/sigstore/rekor/pkg/util"
 "github.com/cyberphone/json-canonicalization/go/src/webpki.org/jsoncanonicalizer"
 "google.golang.org/protobuf/encoding/protojson"
)
func check(err error){if err!=nil{panic(err)}}
func write(path string, data []byte){check(os.WriteFile(path,data,0600))}
func main(){
 if len(os.Args)<3{panic("output chart...")};out:=os.Args[1]
 authority,err:=ca.NewVirtualSigstore();check(err)
 logKey,err:=ecdsa.GenerateKey(elliptic.P256(),rand.Reader);check(err)
 logSigner,err:=signature.LoadECDSASignerVerifier(logKey,crypto.SHA256);check(err)
 logPublic,err:=x509.MarshalPKIXPublicKey(logKey.Public());check(err);logID:=sha256.Sum256(logPublic)
 logs:=map[string]*root.TransparencyLog{hex.EncodeToString(logID[:]):{BaseURL:"https://rekor.kborg.fixture",ID:logID[:],ValidityPeriodStart:time.Now().Add(-time.Hour),ValidityPeriodEnd:time.Now().Add(24*time.Hour),HashFunc:crypto.SHA256,PublicKey:logKey.Public(),SignatureHashFunc:crypto.SHA256}}
 trust,err:=root.NewTrustedRoot(root.TrustedRootMediaType01,authority.FulcioCertificateAuthorities(),authority.CTLogs(),authority.TimestampingAuthorities(),logs);check(err)
 data,err:=json.Marshal(trust);check(err);write(filepath.Join(out,"trusted-root.json"),data)
 foreign,err:=ca.NewVirtualSigstore();check(err)
 wrong,err:=root.NewTrustedRoot(root.TrustedRootMediaType01,foreign.FulcioCertificateAuthorities(),foreign.CTLogs(),foreign.TimestampingAuthorities(),foreign.RekorLogs());check(err)
 data,err=json.Marshal(wrong);check(err);write(filepath.Join(out,"wrong-root.json"),data)
 for index,path:=range os.Args[2:]{
  artifact,err:=os.ReadFile(path);check(err)
  entity,err:=authority.SignAtTime("fixture@invalid.example","https://issuer.invalid.example",artifact,time.Now());check(err)
  content,err:=entity.VerificationContent();check(err);certificate:=content.Certificate()
  public,err:=x509.MarshalPKIXPublicKey(certificate.PublicKey);check(err)
  filename:=fmt.Sprintf("chart-%d",index)
  write(filepath.Join(out,filename+".pub"),pem.EncodeToMemory(&pem.Block{Type:"PUBLIC KEY",Bytes:public}))
  entries,err:=entity.TlogEntries();check(err);entry:=entries[0].TransparencyLogEntry()
  // A key policy uses a public-key log entry, never a certificate signature.
  var body map[string]interface{};check(json.Unmarshal(entry.CanonicalizedBody,&body))
  spec:=body["spec"].(map[string]interface{});signatureObject:=spec["signature"].(map[string]interface{});publicObject:=signatureObject["publicKey"].(map[string]interface{})
  publicObject["content"]=base64.StdEncoding.EncodeToString(pem.EncodeToMemory(&pem.Block{Type:"PUBLIC KEY",Bytes:public}))
  raw,err:=json.Marshal(body);check(err);entry.CanonicalizedBody,err=jsoncanonicalizer.Transform(raw);check(err)
  // Standard Rekor SDK primitives produce a genuine one-leaf signed log
  // checkpoint and inclusion promise. No verifier bypass or custom crypto.
  entry.LogIndex=0;entry.LogId=&common.LogId{KeyId:logID[:]}
  payload,err:=json.Marshal(tlog.RekorPayload{Body:base64.StdEncoding.EncodeToString(entry.CanonicalizedBody),IntegratedTime:entry.IntegratedTime,LogIndex:0,LogID:hex.EncodeToString(logID[:])});check(err)
  canonical,err:=jsoncanonicalizer.Transform(payload);check(err)
  signed,err:=logSigner.SignMessage(bytes.NewReader(canonical));check(err)
  entry.InclusionPromise=&rekor.InclusionPromise{SignedEntryTimestamp:signed}
  hash:=sha256.Sum256(append([]byte{0},entry.CanonicalizedBody...))
  checkpoint,err:=util.CreateAndSignCheckpoint(context.Background(),"rekor.kborg.fixture",0,1,hash[:],logSigner);check(err)
  entry.InclusionProof=&rekor.InclusionProof{LogIndex:0,TreeSize:1,RootHash:hash[:],Checkpoint:&rekor.Checkpoint{Envelope:string(checkpoint)}}
  entry.KindVersion=&rekor.KindVersion{Kind:"hashedrekord",Version:"0.0.1"}
  signature,err:=entity.SignatureContent();check(err);message:=signature.MessageSignatureContent();sum:=sha256.Sum256(artifact)
  hint:=sha256.Sum256(public)
  bundle:=&pb.Bundle{MediaType:"application/vnd.dev.sigstore.bundle.v0.3+json",Content:&pb.Bundle_MessageSignature{MessageSignature:&common.MessageSignature{MessageDigest:&common.HashOutput{Algorithm:common.HashAlgorithm_SHA2_256,Digest:sum[:]},Signature:message.Signature()}},VerificationMaterial:&pb.VerificationMaterial{Content:&pb.VerificationMaterial_PublicKey{PublicKey:&common.PublicKeyIdentifier{Hint:hex.EncodeToString(hint[:])}},TlogEntries:[]*rekor.TransparencyLogEntry{entry}}}
  data,err=protojson.Marshal(bundle);check(err);write(filepath.Join(out,filename+".sigstore.json"),data)
 }
}
'''


def run(argv, env, timeout=60):
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen([str(arg) for arg in argv], stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                   env=env, start_new_session=True)
        try:
            process.wait(timeout=timeout)
        finally:
            import signal
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
        if out.tell() > 8 * 1024 * 1024 or err.tell() > 8 * 1024 * 1024:
            raise RuntimeError("fixture diagnostics exceeded 8 MiB")
        out.seek(0)
        return process.returncode, out.read()


def create(root: Path, charts: dict[str, Path], cosign: Path, go: Path):
    root, cosign, go = Path(root).resolve(), Path(cosign).resolve(), Path(go).resolve()
    if not charts or not cosign.is_file() or not go.is_file():
        raise ValueError("explicit local Cosign/Go binaries and charts are required")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError("Sigstore fixture output must be empty")
    root.chmod(0o700)
    scratch = Path(tempfile.mkdtemp(prefix="sigstore-private-", dir=root))
    (scratch / "tmp").mkdir(mode=0o700)
    for telemetry in (scratch / "Library/Application Support/go/telemetry", scratch / ".config/go/telemetry", scratch / "telemetry"):
        telemetry.mkdir(mode=0o700, parents=True)
        (telemetry / "mode").write_text("off\n")
    cache = root.parent / "sigstore-fixture-go-cache"
    if cache.is_symlink():
        raise ValueError("fixture Go cache may not be a symlink")
    cache.mkdir(mode=0o700, exist_ok=True)
    cache.chmod(0o700)
    env = {"HOME": str(scratch), "GOPATH": str(cache / "gopath"), "GOCACHE": str(cache / "go-cache"),
           "TMPDIR": str(scratch / "tmp"), "PATH": "/usr/bin:/bin", "GOMAXPROCS": "2", "CGO_ENABLED": "0",
           "GOPROXY": "https://proxy.golang.org", "GOSUMDB": "sum.golang.org", "GOFLAGS": "-modcacherw", "GOTOOLCHAIN": "local", "GOTELEMETRY": "off", "GOENV": "off", "TEST_TELEMETRY_DIR": str(scratch / "telemetry")}
    published = []
    try:
        module = scratch / "module"
        module.mkdir(mode=0o700)
        (module / "go.mod").write_text("module kborg-sigstore-fixture\n\ngo 1.26.0\n\nrequire github.com/sigstore/sigstore-go v1.2.2\n")
        (module / "main.go").write_text(SOURCE)
        # -C and explicit cwd keep all build/module state in our new private tree.
        status, _ = run([go, "-C", module, "mod", "tidy"], env, timeout=300)
        if status:
            raise RuntimeError("upstream Sigstore fixture modules failed to resolve; private diagnostics suppressed")
        status, _ = run([go, "-C", module, "build", "-o", scratch / "fixture"], env, timeout=300)
        if status:
            raise RuntimeError("upstream Sigstore fixture build failed; private diagnostics suppressed")
        output = scratch / "public"
        output.mkdir(mode=0o700)
        for name, path in charts.items():
            if not re.fullmatch(r"[a-z][a-z0-9-]{0,40}", name) or not Path(path).is_file() or Path(path).stat().st_size > 8 * 1024 * 1024:
                raise ValueError("fixture chart identity/size invalid")
        status, _ = run([scratch / "fixture", output, *[Path(path).resolve() for path in charts.values()]], env)
        if status:
            raise RuntimeError("upstream Sigstore virtual-service signing failed")
        cosign_env = {"HOME": str(scratch), "TMPDIR": str(scratch), "SIGSTORE_NO_CACHE": "true", "LANG": "C"}
        for index, (name, chart) in enumerate(charts.items()):
            args = [cosign, "verify-blob", "--offline=true", "--bundle", output / f"chart-{index}.sigstore.json",
                    "--trusted-root", output / "trusted-root.json", "--key", output / f"chart-{index}.pub", Path(chart).resolve()]
            status, _ = run(args, cosign_env)
            if status:
                raise RuntimeError("genuine Sigstore signature/log-proof fixture failed trusted Cosign verification")
        for source in output.iterdir():
            target = root / source.name
            with target.open("xb") as stream:
                target.chmod(0o600)
                stream.write(source.read_bytes())
            published.append(target)
        verifier_hash = hashlib.sha256(cosign.read_bytes()).hexdigest()
        policies = {}
        for index, (name, chart) in enumerate(charts.items()):
            chart_path = root / f"{name}.tgz"
            chart_path.write_bytes(Path(chart).read_bytes())
            chart_path.chmod(0o600)
            published.append(chart_path)
            value = {"schema": 1, "executable": str(cosign), "sha256": verifier_hash,
                     "bundle": str(root / f"chart-{index}.sigstore.json"),
                     "trust": {"mode": "public-key", "trusted_root": str(root / "trusted-root.json"), "key": str(root / f"chart-{index}.pub")}}
            policy = root / f"{name}.policy.json"
            policy.write_text(json.dumps(value, indent=2))
            policy.chmod(0o600)
            published.append(policy)
            policies[name] = policy
        manifest = {"charts": {name: f"{name}.tgz" for name in charts}, "policies": {name: path.name for name,path in policies.items()},
                    "verifier_sha256": verifier_hash, "upstream_fixture_sdk": "sigstore-go v1.2.2", "ephemeral_public_test_authority": True}
        (root / "fixtures.json").write_text(json.dumps(manifest, indent=2))
        return manifest
    except BaseException:
        for path in published:
            path.unlink(missing_ok=True)
        raise
    finally:
        shutil.rmtree(scratch)



def create_identity(root: Path, cosign: Path):
    """Verify an authentic public release bundle, never obtain a login.

    This small signed artifact is the official release checksum document, not a
    Helm chart. It exercises certificate identity, issuer, SCT, historic signing
    time and transparency verification independently of chart installation.
    """
    root, cosign = Path(root).resolve(), Path(cosign).resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(root.iterdir()):
        raise ValueError("identity fixture output must be empty")
    root.chmod(0o700)
    scratch=Path(tempfile.mkdtemp(prefix="identity-private-",dir=root))
    published=[]
    sources={
        "chart.tgz": ("https://github.com/sigstore/cosign/releases/download/v3.1.3/cosign_checksums.txt", "aec2a6f68d307b09ae196e388dc691a146fa8bdba7fcce9ca4ca41b918adfa63"),
        "bundle.json": ("https://github.com/sigstore/cosign/releases/download/v3.1.3/cosign_checksums.txt.sigstore.json", "976bcb216e45ed0274e464e2e16d81e84cc85a69b3ed6e3488c1e7cda116379a"),
        "trusted-root.json": ("https://raw.githubusercontent.com/sigstore/root-signing/52bb6ab9d7eb73bb6a6fbd8bb216fb2e9720b10a/targets/trusted_root.json", "6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66"),
    }
    env={"HOME":str(scratch),"TMPDIR":str(scratch),"SIGSTORE_NO_CACHE":"true","LANG":"C"}
    try:
        curl=shutil.which("curl")
        if not curl:raise RuntimeError("bounded public identity fixture acquisition requires curl")
        for name,(url,expected) in sources.items():
            path=root/name
            status,_=run([curl,"--disable","--fail","--silent","--show-error","--location","--max-time","30","--max-filesize","1000000","--proto","=https","--tlsv1.2",url,"-o",path],env,timeout=35)
            published.append(path)
            if status or not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest()!=expected:
                raise RuntimeError("official public identity fixture download/hash verification failed")
            path.chmod(0o600)
        policy={"schema":1,"executable":str(cosign),"sha256":hashlib.sha256(cosign.read_bytes()).hexdigest(),"bundle":str(root/"bundle.json"),
                "trust":{"mode":"identity","trusted_root":str(root/"trusted-root.json"),"identity":"keyless@projectsigstore.iam.gserviceaccount.com","issuer":"https://accounts.google.com"}}
        for name,key,value in [("identity",None,None),("wrong-identity","identity","wrong@invalid.example"),("wrong-issuer","issuer","https://wrong.invalid.example")]:
            candidate=json.loads(json.dumps(policy))
            if key:candidate["trust"][key]=value
            path=root/(name+".policy.json");path.write_text(json.dumps(candidate,indent=2));path.chmod(0o600);published.append(path)
            args=[cosign,"verify-blob","--offline=true","--bundle",root/"bundle.json","--trusted-root",root/"trusted-root.json","--certificate-identity",candidate["trust"]["identity"],"--certificate-oidc-issuer",candidate["trust"]["issuer"],root/"chart.tgz"]
            status,_=run(args,env,timeout=20)
            if (status==0)!=(name=="identity"):
                raise RuntimeError("genuine exact-identity Sigstore positive/negative fixture expectation failed")
        result={"subject":"official Cosign3.1.3 release checksums, not a Helm chart","source_sha256":{name:expected for name,(_,expected) in sources.items()},
                "identity_and_issuer_verified":True,"wrong_identity_and_issuer_refused":True,"no_login_or_signing_request":True}
        (root/"fixtures.json").write_text(json.dumps(result,indent=2))
        return result
    except BaseException:
        for path in published:path.unlink(missing_ok=True)
        raise
    finally:
        shutil.rmtree(scratch)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--cosign",type=Path,required=True)
    parser.add_argument("--go",type=Path,required=True)
    parser.add_argument("--chart",action="append",default=[])
    parser.add_argument("--identity-output",type=Path,help="Also acquire pinned official exact-identity verification fixtures (not chart installation fixtures)")
    args=parser.parse_args()
    charts={}
    for item in args.chart:
        name,separator,path=item.partition("=")
        if not separator or name in charts:parser.error("--chart requires unique NAME=ABS_CHART")
        charts[name]=Path(path)
    print(json.dumps(create(args.output,charts,args.cosign,args.go)))
    if args.identity_output:print(json.dumps(create_identity(args.identity_output,args.cosign)))


if __name__=="__main__":main()
