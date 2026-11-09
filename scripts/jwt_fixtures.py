#!/usr/bin/env python3
"""Generates tests/fixtures/jwt/: test keys and access tokens made by an independent
JOSE implementation (PyJWT and cryptography), which staalc's verifier (src/mcp/jwt.rs)
must judge exactly as the expectations in cases.json say. Run it again only to change
the cases; the keys and tokens are fixed once written.

    python3 scripts/jwt_fixtures.py

The keys are for tests only: they sign nothing anywhere else.
"""
import base64, hashlib, hmac, json, os, sys

import jwt  # PyJWT
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, rsa

OUT = os.path.join(os.path.dirname(__file__), "..", "tests", "fixtures", "jwt")
ISS = "https://auth.test.example"
AUD = "https://mcp.test.example/mcp"
# Tokens are judged at this moment (2030-01-01T00:00:00Z).
NOW = 1893456000


def b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def int_b64u(n: int) -> str:
    return b64u(n.to_bytes((n.bit_length() + 7) // 8, "big"))


def main():
    os.makedirs(OUT, exist_ok=True)
    keys = {
        "rsa": rsa.generate_private_key(public_exponent=65537, key_size=2048),
        "rsa1024": rsa.generate_private_key(public_exponent=65537, key_size=1024),
        "ec": ec.generate_private_key(ec.SECP256R1()),
        "ed": ed25519.Ed25519PrivateKey.generate(),
        "other_ec": ec.generate_private_key(ec.SECP256R1()),
    }
    jwks = []
    for kid, k in keys.items():
        if kid == "other_ec":
            continue  # signs tokens whose key is not published
        pub = k.public_key()
        if isinstance(k, rsa.RSAPrivateKey):
            n = pub.public_numbers()
            jwks.append({"kty": "RSA", "kid": kid, "use": "sig", "n": int_b64u(n.n), "e": int_b64u(n.e)})
        elif isinstance(k, ec.EllipticCurvePrivateKey):
            n = pub.public_numbers()
            jwks.append({"kty": "EC", "kid": kid, "crv": "P-256", "x": b64u(n.x.to_bytes(32, "big")), "y": b64u(n.y.to_bytes(32, "big"))})
        else:
            raw = pub.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
            jwks.append({"kty": "OKP", "kid": kid, "crv": "Ed25519", "x": b64u(raw)})
    # The same RSA key published again for PS256 only, under its own kid.
    rsa_n = keys["rsa"].public_key().public_numbers()
    jwks.append({"kty": "RSA", "kid": "rsa-ps", "alg": "PS256", "n": int_b64u(rsa_n.n), "e": int_b64u(rsa_n.e)})

    def claims(**over):
        c = {"iss": ISS, "aud": AUD, "sub": "user-1", "client_id": "client-1", "jti": "j-1",
             "iat": NOW - 60, "exp": NOW + 3600, "scope": "tools refunds:write"}
        for k, v in over.items():
            if v is None:
                c.pop(k, None)
            else:
                c[k] = v
        return c

    def sign(alg, kid, c=None, h=None, key=None):
        if key is None:
            key = keys[kid] if kid in keys else keys["rsa"]
        headers = h or {}
        h = {"typ": "at+jwt", "kid": kid}
        h.update(headers)
        h = {k: v for k, v in h.items() if v is not None}
        return jwt.encode(c if c is not None else claims(), key, algorithm=alg, headers=h)

    def raw_token(header: dict, payload: dict, sig: bytes) -> str:
        return b64u(json.dumps(header).encode()) + "." + b64u(json.dumps(payload).encode()) + "." + b64u(sig)

    cases = []

    def case(name, token, ok, why=None):
        cases.append({"name": name, "token": token, "ok": ok, "why": why})

    case("RS256", sign("RS256", "rsa"), True)
    case("PS256", sign("PS256", "rsa-ps"), True)
    case("ES256", sign("ES256", "ec"), True)
    case("EdDSA", sign("EdDSA", "ed"), True)
    case("typ application/at+jwt", sign("ES256", "ec", h={"typ": "application/at+jwt"}), True)
    case("aud as an array holding the resource", sign("ES256", "ec", claims(aud=["https://other.example", AUD])), True)
    case("expired within the leeway", sign("ES256", "ec", claims(exp=NOW - 30)), True)
    case("RS256 under the PS256-only key", sign("RS256", "rsa-ps"), False, "alg")
    case("RSA key under 2048 bits", sign("RS256", "rsa1024"), False, "key")
    case("expired", sign("ES256", "ec", claims(exp=NOW - 120)), False, "exp")
    case("not yet valid", sign("ES256", "ec", claims(nbf=NOW + 120)), False, "nbf")
    case("issued in the future", sign("ES256", "ec", claims(iat=NOW + 120)), False, "iat")
    case("another issuer", sign("ES256", "ec", claims(iss="https://evil.example")), False, "iss")
    case("issuer with a trailing slash", sign("ES256", "ec", claims(iss=ISS + "/")), False, "iss")
    case("another audience", sign("ES256", "ec", claims(aud="https://other.example")), False, "aud")
    case("aud array without the resource", sign("ES256", "ec", claims(aud=["https://other.example"])), False, "aud")
    case("no typ", sign("ES256", "ec", h={"typ": None}), False, "typ")
    case("typ JWT", sign("ES256", "ec", h={"typ": "JWT"}), False, "typ")
    for missing in ["sub", "client_id", "jti", "iat", "exp", "iss", "aud"]:
        case(f"no {missing}", sign("ES256", "ec", claims(**{missing: None})), False, missing)
    case("sub not a string", sign("ES256", "ec", claims(sub=42)), False, "sub")
    case("an unknown kid", sign("ES256", "other_ec"), False, "kid")
    case("no kid", sign("ES256", "ec", h={"kid": None}), False, "kid")
    case("crit", sign("ES256", "ec", h={"crit": ["exp"]}), False, "crit")
    case("a key in the header (jwk)", sign("ES256", "ec", h={"jwk": {"kty": "EC"}}, key=keys["other_ec"]), False, "signature")
    case("jku", sign("ES256", "ec", h={"jku": "https://evil.example/keys"}), True)  # never used to find a key
    # alg none, and HS256 keyed with the RSA public key's bytes (algorithm confusion),
    # built by hand: PyJWT refuses to make either.
    case("alg none", raw_token({"alg": "none", "typ": "at+jwt", "kid": "rsa"}, claims(), b""), False, "alg")
    pem = keys["rsa"].public_key().public_bytes(serialization.Encoding.PEM, serialization.PublicFormat.SubjectPublicKeyInfo)
    h = {"alg": "HS256", "typ": "at+jwt", "kid": "rsa"}
    signing_input = (b64u(json.dumps(h).encode()) + "." + b64u(json.dumps(claims()).encode())).encode()
    case("HS256 with the RSA public key as secret", signing_input.decode() + "." + b64u(hmac.new(pem, signing_input, hashlib.sha256).digest()), False, "alg")
    good = sign("ES256", "ec")
    head, body, sig = good.split(".")
    flipped = bytearray(base64.urlsafe_b64decode(sig + "=" * (-len(sig) % 4)))
    flipped[0] ^= 1
    case("a signature with one bit changed", f"{head}.{body}.{b64u(bytes(flipped))}", False, "signature")
    case("a claim changed after signing", f"{head}.{b64u(json.dumps(claims(sub='admin')).encode())}.{sig}", False, "signature")
    case("padding in base64url", f"{head}=.{body}.{sig}", False, "format")
    case("four parts", good + ".x", False, "format")
    case("ES256 signed by the Ed25519 key's kid", sign("ES256", "ed", key=keys["ec"]), False, "alg")

    with open(os.path.join(OUT, "jwks.json"), "w") as f:
        json.dump({"keys": jwks}, f, indent=1)
    with open(os.path.join(OUT, "cases.json"), "w") as f:
        json.dump({"issuer": ISS, "audience": AUD, "now": NOW, "leeway_s": 60,
                   "generated_by": f"PyJWT {jwt.__version__}, cryptography (scripts/jwt_fixtures.py)", "cases": cases}, f, indent=1)
    print(f"{len(cases)} cases, {len(jwks)} keys in {os.path.normpath(OUT)}")


if __name__ == "__main__":
    sys.exit(main())
