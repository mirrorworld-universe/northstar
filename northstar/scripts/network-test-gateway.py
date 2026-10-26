#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["grpcio>=1.78", "grpcio-tools>=1.78", "zstandard>=0.25"]
# ///
"""Loopback-only SP1 6.8.0 reserved API fixture, backed by real CUDA proving.

Not a Succinct node: no authentication, auction, billing, or production privacy.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid

MAX_ARTIFACT = 64 * 1024 * 1024
MAX_WITNESS = 4 * 1024 * 1024


def onchain_key(network_hash):
    # SP1's network key keeps eight u32 words; its BN254 key packs the same words at 31-bit offsets.
    if len(network_hash) != 32:
        raise ValueError("invalid SP1 network key length")
    value = 0
    for word in struct.unpack(">8I", network_hash):
        if word >= 0x7f000001:
            raise ValueError("noncanonical SP1 key word")
        value = (value << 31) + word
    return "0x" + value.to_bytes(32, "big").hex()


def decode_vec(encoded):
    if len(encoded) < 8:
        raise ValueError("truncated bincode vector")
    size, = struct.unpack_from("<Q", encoded)
    if size != len(encoded) - 8:
        raise ValueError("bincode vector length mismatch")
    return encoded[8:]


def decode_stdin(encoded):
    if len(encoded) < 32:
        raise ValueError("truncated SP1 stdin")
    count, size = struct.unpack_from("<QQ", encoded)
    if count != 1 or size > MAX_WITNESS or len(encoded) != size + 32:
        raise ValueError("expected exactly one bounded SP1 stdin buffer")
    pointer, proofs = struct.unpack_from("<QQ", encoded, 16 + size)
    if pointer != 0 or proofs != 0:
        raise ValueError("unsupported SP1 stdin cursor or recursive proofs")
    return encoded[16:16 + size]


class Gateway:
    def __init__(self, args, proto, grpc):
        self.args, self.proto, self.grpc = args, proto, grpc
        self.root = Path(args.state).resolve()
        self.root.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.manifest = json.loads(Path(args.manifest).read_text())
        self.base = f"http://127.0.0.1:{args.http_port}"
        self.lock = threading.Lock()
        self.artifacts, self.jobs, self.calls = {}, {}, {}
        self.program = None
        self.process = None
        self.pool = ThreadPoolExecutor(max_workers=1)

    def count(self, method):
        with self.lock:
            self.calls[method] = self.calls.get(method, 0) + 1
            (self.root / "calls.json").write_text(json.dumps(self.calls, sort_keys=True))

    def artifact_path(self, uri):
        prefix = self.base + "/artifacts/"
        if not uri.startswith(prefix):
            raise ValueError("artifact URI is not owned by this gateway")
        name = uri[len(prefix):]
        if len(name) != 32 or any(c not in "0123456789abcdef" for c in name):
            raise ValueError("invalid artifact identifier")
        with self.lock:
            if name not in self.artifacts:
                raise ValueError("unknown artifact")
        return self.root / (name + ".bin")

    def content(self, uri):
        import zstandard
        path = self.artifact_path(uri)
        with path.open("rb") as file:
            with zstandard.ZstdDecompressor().stream_reader(file) as reader:
                encoded = reader.read(MAX_ARTIFACT + 1)
        if len(encoded) > MAX_ARTIFACT:
            raise ValueError("decompressed artifact exceeds limit")
        return encoded

    def create_artifact(self, request, context):
        self.count("CreateArtifact")
        if request.artifact_type not in (1, 7):
            context.abort(self.grpc.StatusCode.INVALID_ARGUMENT, "only program/private stdin supported")
        name = uuid.uuid4().hex
        with self.lock:
            self.artifacts[name] = request.artifact_type
        uri = self.base + "/artifacts/" + name
        return self.proto.CreateArtifactResponse(artifact_uri=uri, artifact_presigned_url=uri)

    def get_nonce(self, request, context):
        self.count("GetNonce")
        return self.proto.GetNonceResponse(nonce=0)

    def get_balance(self, request, context):
        self.count("GetBalance")
        return self.proto.GetBalanceResponse(amount="0")

    def get_program(self, request, context):
        self.count("GetProgram")
        if self.program is None or request.vk_hash != self.program.vk_hash:
            context.abort(self.grpc.StatusCode.NOT_FOUND, "program not registered")
        return self.proto.GetProgramResponse(program=self.program)

    def create_program(self, request, context):
        self.count("CreateProgram")
        try:
            body = request.body
            elf = decode_vec(self.content(body.program_uri))
            if hashlib.sha256(elf).hexdigest() != self.manifest["elf_sha256"]:
                raise ValueError("program ELF mismatch")
            if onchain_key(body.vk_hash) != self.manifest["program_vkey_hash"]:
                raise ValueError("program key mismatch")
            self.program = self.proto.Program(vk_hash=body.vk_hash, vk=body.vk, program_uri=body.program_uri)
            return self.proto.CreateProgramResponse(tx_hash=bytes(32), body=self.proto.Empty())
        except (OSError, ValueError) as error:
            context.abort(self.grpc.StatusCode.INVALID_ARGUMENT, str(error))

    def request_proof(self, request, context):
        self.count("RequestProof")
        try:
            body = request.body
            if self.program is None or body.vk_hash != self.program.vk_hash:
                raise ValueError("unknown program")
            if body.mode != 4 or body.strategy != 2 or not body.stdin_private:
                raise ValueError("only reserved Groth16 with private stdin supported")
            if not time.time() < body.deadline <= time.time() + 120:
                raise ValueError("request deadline must be within 120 seconds")
            witness = decode_stdin(self.content(body.stdin_uri))
            with self.lock:
                if any(job["status"] == 2 for job in self.jobs.values()):
                    context.abort(self.grpc.StatusCode.RESOURCE_EXHAUSTED, "GPU busy")
                request_id = uuid.uuid4().bytes + uuid.uuid4().bytes
                directory = self.root / request_id.hex()
                directory.mkdir(mode=0o700)
                (directory / "witness.bin").write_bytes(witness)
                job = {"status": 2, "deadline": body.deadline, "directory": directory}
                self.jobs[request_id] = job
            (directory / "request.json").write_text(json.dumps({
                "request_id": request_id.hex(), "version": body.version,
                "witness_sha256": hashlib.sha256(witness).hexdigest(),
                "mode": "groth16", "strategy": "reserved", "funds_required": False,
                "cycle_limit": body.cycle_limit, "gas_limit": body.gas_limit,
            }, sort_keys=True))
            self.pool.submit(self.prove, request_id, job)
            return self.proto.RequestProofResponse(tx_hash=bytes(32), body=self.proto.RequestProofResponseBody(request_id=request_id))
        except (OSError, ValueError) as error:
            context.abort(self.grpc.StatusCode.INVALID_ARGUMENT, str(error))

    def prove(self, request_id, job):
        started = time.monotonic()
        directory = job["directory"]
        process = None
        try:
            command = [self.args.wrapper, self.args.prover, "groth16", str(directory / "witness.bin"),
                       str(directory / "measurements.json"), "local-hosted-network"]
            with (directory / "prover.log").open("wb") as log:
                process = subprocess.Popen(command, cwd=directory, stdout=log, stderr=subprocess.STDOUT,
                                           start_new_session=True, env={**os.environ, "SP1_PROVER": "cuda"})
                self.process = process
                status = process.wait(timeout=max(1, job["deadline"] - time.time()))
            if status:
                raise RuntimeError(f"CUDA backend exited {status}")
            measurements = json.loads((directory / "measurements.json").read_text())
            if measurements["program_vkey_hash"] != self.manifest["program_vkey_hash"]:
                raise ValueError("CUDA backend used a different guest")
            if len((directory / "northstar-sp1-groth16-onchain.bin").read_bytes()) != 356:
                raise ValueError("CUDA backend returned wrong proof envelope")
            with self.lock:
                job["status"] = 3
            outcome = {"ok": True, "wall_ms": round((time.monotonic() - started) * 1000)}
        except Exception as error:
            with self.lock:
                job["status"] = 4
            outcome = {"ok": False, "error": str(error)}
        finally:
            if process is not None and process.poll() is None:
                self.stop_process(process)
            self.process = None
        (directory / "outcome.json").write_text(json.dumps(outcome, sort_keys=True))
        print(json.dumps({"request_id": request_id.hex(), **outcome}), flush=True)

    @staticmethod
    def stop_process(process):
        try:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        except ProcessLookupError:
            pass

    def get_status(self, request, context):
        self.count("GetProofRequestStatus")
        with self.lock:
            job = self.jobs.get(request.request_id)
            if job is None:
                context.abort(self.grpc.StatusCode.NOT_FOUND, "unknown request")
            status, deadline = job["status"], job["deadline"]
        response = self.proto.GetProofRequestStatusResponse(
            fulfillment_status=status, execution_status=2 if status == 3 else 1,
            request_tx_hash=bytes(32), deadline=deadline)
        if status == 3:
            response.proof_uri = self.base + "/proofs/" + request.request_id.hex()
        return response

    def handlers(self):
        methods = {
            "GetNonce": (self.get_nonce, self.proto.GetNonceRequest, self.proto.GetNonceResponse),
            "GetBalance": (self.get_balance, self.proto.GetBalanceRequest, self.proto.GetBalanceResponse),
            "GetProgram": (self.get_program, self.proto.GetProgramRequest, self.proto.GetProgramResponse),
            "CreateProgram": (self.create_program, self.proto.CreateProgramRequest, self.proto.CreateProgramResponse),
            "RequestProof": (self.request_proof, self.proto.RequestProofRequest, self.proto.RequestProofResponse),
            "GetProofRequestStatus": (self.get_status, self.proto.GetProofRequestStatusRequest, self.proto.GetProofRequestStatusResponse),
        }
        def handler(function, request, response):
            return self.grpc.unary_unary_rpc_method_handler(function, request_deserializer=request.FromString,
                                                            response_serializer=response.SerializeToString)
        return (
            self.grpc.method_handlers_generic_handler("network.ProverNetwork", {
                name: handler(*method) for name, method in methods.items()}),
            self.grpc.method_handlers_generic_handler("artifact.ArtifactStore", {
                "CreateArtifact": handler(self.create_artifact, self.proto.CreateArtifactRequest, self.proto.CreateArtifactResponse)}),
        )


def http_handler(gateway):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            if self.path == "/healthz":
                encoded = b"ok\n"
            elif self.path.startswith("/proofs/"):
                name = self.path[len("/proofs/"):]
                try:
                    identifier = bytes.fromhex(name)
                    with gateway.lock:
                        job = gateway.jobs.get(identifier)
                        if job is None or job["status"] != 3:
                            raise ValueError("proof not ready")
                    encoded = (job["directory"] / "northstar-sp1-groth16.bin").read_bytes()
                    gateway.count("DownloadProof")
                except (ValueError, OSError):
                    self.send_error(404)
                    return
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_PUT(self):
            try:
                size = int(self.headers.get("Content-Length", "-1"))
                if not 0 <= size <= MAX_ARTIFACT:
                    raise ValueError("invalid upload size")
                path = gateway.artifact_path(gateway.base + self.path)
                encoded = self.rfile.read(size)
                if len(encoded) != size:
                    raise ValueError("incomplete upload")
                path.write_bytes(encoded)
                gateway.count("UploadArtifact")
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()
            except (ValueError, OSError):
                self.send_error(400)
    return Handler


def main():
    import grpc
    from grpc_tools import protoc
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", required=True)
    parser.add_argument("--prover", required=True)
    parser.add_argument("--wrapper", required=True)
    parser.add_argument("--manifest", default=str(Path(__file__).resolve().parents[1] / "zkvm-replay/partial-candidate-v1.json"))
    parser.add_argument("--grpc-port", type=int, default=50051)
    parser.add_argument("--http-port", type=int, default=50052)
    args = parser.parse_args()
    os.umask(0o077)
    schema = Path(__file__).with_name("network_test.proto")
    with tempfile.TemporaryDirectory() as generated:
        if protoc.main(["protoc", f"-I{schema.parent}", f"--python_out={generated}", str(schema)]):
            raise RuntimeError("test protocol generation failed")
        sys.path.insert(0, generated)
        import network_test_pb2
        gateway = Gateway(args, network_test_pb2, grpc)
        http = ThreadingHTTPServer(("127.0.0.1", args.http_port), http_handler(gateway))
        http_thread = threading.Thread(target=http.serve_forever, daemon=True)
        server = grpc.server(ThreadPoolExecutor(max_workers=4))
        server.add_generic_rpc_handlers(gateway.handlers())
        if server.add_insecure_port(f"127.0.0.1:{args.grpc_port}") == 0:
            raise RuntimeError("could not bind loopback gRPC port")
        server.start()
        http_thread.start()
        def interrupted(_signum, _frame):
            raise KeyboardInterrupt
        signal.signal(signal.SIGTERM, interrupted)
        (gateway.root / "ready.json").write_text(json.dumps({"rpc_url": f"http://127.0.0.1:{args.grpc_port}", "test_only": True}))
        print("test gateway ready", flush=True)
        try:
            server.wait_for_termination()
        except KeyboardInterrupt:
            pass
        finally:
            server.stop(0)
            if gateway.process is not None:
                gateway.stop_process(gateway.process)
            gateway.pool.shutdown(wait=True, cancel_futures=True)
            http.shutdown()
            http.server_close()


if __name__ == "__main__":
    main()
