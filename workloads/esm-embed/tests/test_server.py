import base64
import http.client
import json
import threading
from contextlib import contextmanager

import numpy as np
import pytest

from esm_embed import server

SECRET = "MKTAYIAKQRQISFVKSHFSRQLEERLGLIEVQ"  # stands in for unpublished lab data


class FakeModel:
    """Deterministic stand-in: vector = (length, count of A, count of K, 1)."""

    dim = 4
    max_residues = 50

    def embed(self, sequences):
        return np.array(
            [[len(s), s.count("A"), s.count("K"), 1.0] for s in sequences], dtype=np.float32
        )


def decode(payload):
    raw = base64.b64decode(payload["embeddings"])
    return np.frombuffer(raw, dtype="<f4").reshape(payload["count"], payload["dim"])


def body(seqs):
    return json.dumps({"sequences": seqs}).encode()


@contextmanager
def running(service):
    httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.make_handler(service))
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    try:
        yield httpd.server_address[1]
    finally:
        httpd.shutdown()
        httpd.server_close()


def request(port, method, path, data=None, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    conn.request(method, path, body=data, headers=headers or {})
    resp = conn.getresponse()
    out = resp.status, json.loads(resp.read() or b"{}")
    conn.close()
    return out


def ready_service(**limits):
    s = server.Service("fake", server.Limits(**limits))
    s.load(FakeModel)
    return s


# --- validation ----------------------------------------------------------------

@pytest.mark.parametrize(
    "payload, status",
    [
        (b"not json", 400),
        (json.dumps({"sequences": ["MK"], "extra": 1}).encode(), 400),
        (json.dumps({"sequences": []}).encode(), 400),
        (body(["MK", ""]), 422),
        (body(["MK", 7]), 422),
        (body(["MK", "M" * 51]), 422),
        (body(["MK", "MK1"]), 422),
        (body(["MK"] * 9), 413),
    ],
)
def test_rejects_bad_batches(payload, status):
    with pytest.raises(server.RequestError) as exc:
        server.validate(payload, server.Limits(max_sequences=8), max_residues=50)
    assert exc.value.status == status


def test_error_names_position_not_content():
    with pytest.raises(server.RequestError) as exc:
        server.validate(body(["MK", SECRET + "1"]), server.Limits(), max_residues=1000)
    assert "sequence 1" in exc.value.message
    assert SECRET not in exc.value.message


def test_padded_token_limit():
    # 4 sequences x (30 + 2) = 128 padded tokens
    seqs = ["M" * 30, "M", "M", "M"]
    server.validate(body(seqs), server.Limits(max_batch_tokens=128), max_residues=50)
    with pytest.raises(server.RequestError) as exc:
        server.validate(body(seqs), server.Limits(max_batch_tokens=127), max_residues=50)
    assert exc.value.status == 413


# --- service -------------------------------------------------------------------

def test_not_ready_until_loaded():
    s = server.Service("fake", server.Limits())
    assert not s.ready
    with pytest.raises(server.RequestError) as exc:
        s.run(body(["MK"]))
    assert exc.value.status == 503


def test_returns_rows_in_request_order():
    seqs = ["MKKA", "M", "AAAAAAAAAA"]
    out = ready_service().run(body(seqs))
    assert (out["model"], out["dim"], out["count"]) == ("fake", 4, 3)
    np.testing.assert_array_equal(decode(out), FakeModel().embed(seqs))


# --- HTTP ----------------------------------------------------------------------

def test_http_round_trip():
    with running(ready_service()) as port:
        assert request(port, "GET", "/healthz")[0] == 200
        assert request(port, "GET", "/readyz") == (200, {"status": "ready", "model": "fake"})
        status, payload = request(port, "POST", "/v1/batch", body(["MKA", "MA"]))
        assert status == 200
        assert decode(payload)[:, 0].tolist() == [3.0, 2.0]
        assert request(port, "GET", "/nope")[0] == 404
        assert request(port, "POST", "/v1/other", body(["MK"]))[0] == 404


def test_http_reports_loading():
    with running(server.Service("fake", server.Limits())) as port:
        assert request(port, "GET", "/healthz")[0] == 200
        assert request(port, "GET", "/readyz")[0] == 503
        assert request(port, "POST", "/v1/batch", body(["MK"]))[0] == 503


def test_http_error_does_not_echo_input():
    with running(ready_service()) as port:
        status, payload = request(port, "POST", "/v1/batch", body([SECRET + "!"]))
    assert status == 422
    assert SECRET not in json.dumps(payload)


def test_http_oversized_body_rejected_unread():
    with running(ready_service(max_body_bytes=100)) as port:
        status, _ = request(port, "POST", "/v1/batch", body(["M" * 200]))
    assert status == 413


def test_model_failure_is_500_without_details(caplog):
    class Broken(FakeModel):
        def embed(self, sequences):
            raise RuntimeError(f"tensor built from {sequences[0]}")

    s = server.Service("fake", server.Limits())
    s.load(Broken)
    with running(s) as port:
        status, payload = request(port, "POST", "/v1/batch", body([SECRET]))
    assert (status, payload) == (500, {"error": "internal error"})
    assert SECRET not in caplog.text


@pytest.mark.model
def test_real_model_matches_direct_call():
    from esm_embed.model import Embedder

    s = server.Service("esm2", server.Limits())
    s.load(lambda: Embedder("facebook/esm2_t6_8M_UR50D", threads=2))
    seqs = ["MKTAYIAKQRQISFVKSHFSRQ", "MSEQNNTEMTFQIQRIYTKDISFEAPNAPHVFQKDW", "MA"]
    with running(s) as port:
        status, payload = request(port, "POST", "/v1/batch", body(seqs))
    assert status == 200
    np.testing.assert_allclose(decode(payload), s.model.embed(seqs), rtol=1e-5, atol=1e-6)
