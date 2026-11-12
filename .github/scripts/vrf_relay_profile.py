"""Sonic: Immutable remote-only relay configuration for retained native upgrades."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
from continuity_rollout_state import canonical, immutable_json, require

PROFILE = Path("/solana/northstar/vrf-isolated/profile.json")
JOURNAL = Path("/solana/northstar/vrf-isolated/relay-journal")
FULFILLER = Path("/etc/northstar-vrf-relay/fulfiller.json")
BEARER = Path("/etc/northstar-vrf-relay/bearer")
CREDENTIALS = "/run/northstar-vrf-relay/"
FLAG = "--vrf-relay-config"


def read_profile(path, expected):
    require(isinstance(expected,str) and re.fullmatch("[0-9a-f]{64}",expected),"relay source digest is required")
    descriptor=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(descriptor,"rb") as stream:
        info=os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and not info.st_mode & 0o077,"relay source must be private regular state")
        raw=stream.read(65537)
    require(len(raw)<=65536 and hashlib.sha256(raw).hexdigest()==expected,"relay source digest differs")
    value=json.loads(raw)
    require(isinstance(value,dict) and set(value)=={"version","context","configuration"}
            and type(value["version"]) is int and value["version"]==1,"relay source schema differs")
    return value


def validate(profile, identity, genesis, l1_rpc, er_rpc):
    require(profile.get("version")==1 and profile.get("context")==identity["context"],"relay retained context differs")
    config=profile.get("configuration")
    require(isinstance(config,dict) and set(config)=={"version","worker","signer"} and config["version"]==1,"relay configuration schema differs")
    worker,signer=config["worker"],config["signer"]
    allowed={"network","rpc_url","ws_url","expected_genesis","l1_rpc_url","er_session","er_session_context","coordinator","verifier","coordinator_programdata_sha256","verifier_programdata_sha256","vrf_public_key","fulfiller","signer_file","state_dir","commitment","allowed_consumers","request_addresses","allow_store","poll_ms","max_attempts","max_records"}
    require(isinstance(worker,dict) and allowed-{"ws_url","request_addresses"} <= set(worker) <= allowed
            and "vrf_key_file" not in worker,"relay cannot receive a VRF credential or incomplete settings")
    require(worker.get("network")=="devnet_er" and worker.get("rpc_url")==er_rpc and worker.get("l1_rpc_url")==l1_rpc
            and worker.get("expected_genesis")==genesis and worker.get("er_session")==identity["session"]
            and worker.get("er_session_context")==identity["context"] and not worker.get("ws_url") and not worker.get("request_addresses"),"relay endpoint/session binding differs")
    require(isinstance(worker.get("allowed_consumers"),list),"relay callback list differs")
    roles=[worker.get(name) for name in ("coordinator","verifier","vrf_public_key","fulfiller")]+worker["allowed_consumers"]
    alphabet="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    for address in roles:
        require(isinstance(address,str) and 32<=len(address)<=44 and all(c in alphabet for c in address),"relay public identity differs")
        number=0
        for char in address: number=number*58+alphabet.index(char)
        require(number>0 and (number.bit_length()+7)//8+len(address)-len(address.lstrip("1"))==32,"relay public identity must encode 32 nonzero bytes")
    require(len(set(roles))==len(roles) and worker["coordinator"]!="9XHn4tU6H6gibio4omC9Xs2ZCXyqrhTzY3FRidsHuGgm", "relay requires dedicated identities, not the public L1 coordinator")
    require(worker.get("signer_file")==CREDENTIALS+"vrf-relay-fulfiller" and worker.get("state_dir")==str(JOURNAL)
            and worker.get("fulfiller")!=worker.get("vrf_public_key") and worker.get("commitment")=="confirmed","relay wallet/journal policy differs")
    require(worker.get("allow_store") is False and isinstance(worker.get("allowed_consumers"),list) and len(worker["allowed_consumers"])==1
            and type(worker.get("poll_ms")) is int and 2000<=worker["poll_ms"]<=10000
            and type(worker.get("max_attempts")) is int and 1<=worker["max_attempts"]<=2
            and type(worker.get("max_records")) is int and 1<=worker["max_records"]<=128,"relay callback/delivery policy differs")
    require(isinstance(signer,dict) and set(signer)=={"url","bearer_token_file","policy_sha256"}
            and signer["url"]=="http://127.0.0.1:9030" and signer["bearer_token_file"]==CREDENTIALS+"vrf-relay-bearer"
            and re.fullmatch("[0-9a-f]{64}",signer["policy_sha256"] or ""),"isolated signer binding differs")
    for name in ("coordinator_programdata_sha256","verifier_programdata_sha256"):
        require(re.fullmatch("[0-9a-f]{64}",worker.get(name,"")),"relay program digest differs")
    return config


def arguments(original, config, previous=None):
    require(Path(config).is_absolute(),"managed relay configuration path must be absolute")
    result=[]; found=[]; index=0
    while index<len(original):
        value=original[index]
        if value==FLAG or value.startswith(FLAG+"="):
            width=2 if value==FLAG else 1
            require(width==1 or index+1<len(original),"relay argument is missing")
            found.append(original[index+1] if width==2 else value.split("=",1)[1]); index+=width
        else: result.append(value); index+=1
    require(found==([str(previous)] if previous else []),"unknown or duplicated retained relay configuration")
    return [*result,FLAG,str(config)]


def hardening():
    return ("NoNewPrivileges=yes\nCapabilityBoundingSet=\nAmbientCapabilities=\n"
            "LoadCredential=vrf-relay-fulfiller:"+str(FULFILLER)+"\n"
            "LoadCredential=vrf-relay-bearer:"+str(BEARER)+"\n"
            "RuntimeDirectory=northstar-vrf-relay\nRuntimeDirectoryMode=0700\n"
            "ExecStartPre=/usr/bin/install -m 0400 %d/vrf-relay-fulfiller %t/northstar-vrf-relay/vrf-relay-fulfiller\n"
            "ExecStartPre=/usr/bin/install -m 0400 %d/vrf-relay-bearer %t/northstar-vrf-relay/vrf-relay-bearer\n"
            "InaccessiblePaths=-/etc/northstar-vrf-request-signer -/run/credentials/northstar-vrf-request-signer.service -/run/northstar-vrf-request-signer "
            "-/var/lib/private/northstar-vrf-request-signer -/run/docker.sock -/run/podman/podman.sock -/nix/var/nix/daemon-socket\n")


def configuration_hash(profile):
    return hashlib.sha256(canonical(profile["configuration"])+b"\n").hexdigest()


def preflight(upgrade, profile):
    sources=upgrade.inspect([FULFILLER,BEARER])
    require(all(sources[str(path)].get("mode")==0o400 and sources[str(path)].get("uid")==0 for path in (FULFILLER,BEARER)),"relay credentials must be root-owned private regular files")
    values=upgrade.ops.execute(["systemctl","show","--property=DynamicUser,ActiveState,SubState,MainPID,Restart,NRestarts","northstar-vrf-request-signer.service"])
    properties=dict(line.split("=",1) for line in values.splitlines() if "=" in line)
    require(properties.get("DynamicUser")=="yes" and properties.get("ActiveState")=="active" and properties.get("SubState")=="running"
            and properties.get("Restart")=="no" and properties.get("NRestarts")=="0" and properties.get("MainPID", "0").isdigit()
            and int(properties["MainPID"])>0,"isolated signer service is not independently ready")
    info=json.loads(upgrade.ops.execute(["sudo","-n","python3","-B",Path(__file__).resolve(),"info"]))
    require(info.get("version")==1 and info.get("public_key")==profile["configuration"]["worker"]["vrf_public_key"]
            and info.get("policy_sha256")==profile["configuration"]["signer"]["policy_sha256"]
            and info.get("commitment")=="finalized_portal_checkpoint_membership" and info.get("arbitrary_alpha") is False,"isolated signer policy differs")


def install(upgrade):
    profile=upgrade.plan.get("vrf_relay")
    if profile is None:return
    uid,gid=str(upgrade.plan["before"]["uid"]),str(upgrade.plan["before"]["gid"])
    sources=upgrade.inspect([FULFILLER,BEARER])
    require(all(sources[str(path)].get("mode")==0o400 and sources[str(path)].get("uid")==0 for path in (FULFILLER,BEARER)),"relay credentials must be root-owned private regular files")
    path=upgrade.runtime/"vrf-relay.json"
    source=upgrade.control/"vrf-relay.json"
    immutable_json(source,profile["configuration"])
    observed=upgrade.inspect([path,JOURNAL])
    if observed[str(path)].get("absent"):
        upgrade.ops.execute(["sudo","-n","install","-m","400","-o",uid,"-g",gid,source,path])
    else:
        require(observed[str(path)].get("sha256")==configuration_hash(profile) and observed[str(path)].get("uid")==int(uid)
                and observed[str(path)].get("mode")==0o400,"installed relay configuration differs")
    if observed[str(JOURNAL)].get("absent"):
        upgrade.ops.execute(["sudo","-n","install","-d","-m","700","-o",uid,"-g",gid,JOURNAL])
    else:
        require(observed[str(JOURNAL)].get("directory") is True and observed[str(JOURNAL)].get("uid")==int(uid)
                and observed[str(JOURNAL)].get("mode")==0o700,"relay journal ownership differs")
    upgrade.ops.flush([path,source,JOURNAL,JOURNAL.parent])


def verify(upgrade):
    profile=upgrade.plan.get("vrf_relay")
    if profile is None:return
    path=upgrade.runtime/"vrf-relay.json"
    value=upgrade.inspect([path])[str(path)]
    require(value.get("sha256")==configuration_hash(profile) and value.get("mode")==0o400
            and value.get("uid")==upgrade.plan["before"]["uid"],"managed relay configuration changed")
    effective=upgrade.ops.execute(["systemctl","show","--property=NoNewPrivileges","--value",upgrade.service]).strip()
    require(effective=="yes","relay Native unit must forbid privilege elevation")


def public_info():
    import urllib.request
    descriptor=os.open(BEARER,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(descriptor,"rb") as stream:
        info=os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid==0 and stat.S_IMODE(info.st_mode)==0o400,"signer bearer source differs")
        token=stream.read(257).strip()
    require(32<=len(token)<=256 and all(33<=value<=126 for value in token),"signer bearer credential differs")
    request=urllib.request.Request("http://127.0.0.1:9030/v1/info",headers={"Authorization":"Bearer "+token.decode("ascii")})
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request,timeout=5) as response: raw=response.read(4097)
    require(len(raw)<=4096,"signer info exceeds bound")
    value=json.loads(raw)
    require(isinstance(value,dict) and set(value)=={"version","public_key","policy_sha256","commitment","arbitrary_alpha"},"signer info schema differs")
    return value


if __name__=="__main__":
    import sys
    require(sys.argv[1:]==["info"],"only read-only public signer metadata is available")
    print(json.dumps(public_info()))
