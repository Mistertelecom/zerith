#!/usr/bin/env node
// Ed25519 signing and verification tool for Zerith Desktop auto-updates.
// Uses only Node.js built-in modules (Node 18+ / 22+ `node:crypto`).
//
// Canonical signed payload (must match `crates/zerith-desktop/src/update.rs`):
//   zerith-update-v1\n
//   version=<semver>\n
//   file=<basename>\n
//   sha256=<64-hex-lowercase>\n
//
// Commands:
//   node scripts/sign-update.mjs keygen
//   node scripts/sign-update.mjs pubkey
//   node scripts/sign-update.mjs sign <installer-path> <version>
//   node scripts/sign-update.mjs verify <installer-path> <version> <sig-path> [pubkey-b64-or-file]

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

// ASN.1 DER prefix for an Ed25519 SubjectPublicKeyInfo (RFC 8410).
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function normalizeVersion(raw) {
  let v = raw.trim();
  if (v.startsWith("zerith-")) v = v.slice("zerith-".length);
  if (v.startsWith("v")) v = v.slice(1);
  if (!/^\d+\.\d+\.\d+$/.test(v)) {
    throw new Error(`versão inválida para assinatura: '${raw}' (esperado X.Y.Z)`);
  }
  return v;
}

function signingPayload(version, fileName, sha256Hex) {
  return Buffer.from(
    `zerith-update-v1\nversion=${version}\nfile=${fileName}\nsha256=${sha256Hex}\n`,
    "utf8",
  );
}

function sha256File(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

function loadPrivateKey(raw) {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) {
    throw new Error(
      "defina ZERITH_UPDATE_SIGNING_KEY com a chave privada Ed25519 (base64 PKCS#8 ou PEM)",
    );
  }
  const key = trimmed.includes("BEGIN PRIVATE KEY")
    ? crypto.createPrivateKey({ key: trimmed, format: "pem" })
    : crypto.createPrivateKey({
        key: Buffer.from(trimmed.replace(/\s+/g, ""), "base64"),
        format: "der",
        type: "pkcs8",
      });
  if (key.asymmetricKeyType !== "ed25519") {
    throw new Error(`chave privada inválida: esperado ed25519, recebido ${key.asymmetricKeyType}`);
  }
  return key;
}

function rawPubkeyFromKeyObject(pubKey) {
  const der = pubKey.export({ format: "der", type: "spki" });
  if (
    der.length !== ED25519_SPKI_PREFIX.length + 32 ||
    !der.subarray(0, ED25519_SPKI_PREFIX.length).equals(ED25519_SPKI_PREFIX)
  ) {
    throw new Error("chave pública SPKI Ed25519 inesperada");
  }
  return der.subarray(ED25519_SPKI_PREFIX.length);
}

function parsePubkeyText(text) {
  const body = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith("#"))
    .join("");
  if (!body) {
    throw new Error("chave pública vazia");
  }
  const raw = Buffer.from(body, "base64");
  if (raw.length !== 32) {
    throw new Error(`chave pública deve ter 32 bytes em base64 (recebido ${raw.length})`);
  }
  return raw;
}

function loadPubkey(arg) {
  if (arg) {
    if (fs.existsSync(arg)) return parsePubkeyText(fs.readFileSync(arg, "utf8"));
    return parsePubkeyText(arg);
  }
  if (process.env.ZERITH_UPDATE_PUBKEY?.trim()) {
    return parsePubkeyText(process.env.ZERITH_UPDATE_PUBKEY);
  }
  const defaultFile = path.resolve("crates/zerith-desktop/update-pubkey.txt");
  return parsePubkeyText(fs.readFileSync(defaultFile, "utf8"));
}

function cmdKeygen() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ed25519");
  const pubB64 = rawPubkeyFromKeyObject(publicKey).toString("base64");
  const privDerB64 = privateKey.export({ format: "der", type: "pkcs8" }).toString("base64");
  console.log("# 1) Salve esta chave PÚBLICA em crates/zerith-desktop/update-pubkey.txt:");
  console.log(`ZERITH_UPDATE_PUBKEY=${pubB64}`);
  console.log("");
  console.log("# 2) Salve esta chave PRIVADA nos Secrets do GitHub (NUNCA commite no Git):");
  console.log(`ZERITH_UPDATE_SIGNING_KEY=${privDerB64}`);
}

function cmdPubkey() {
  const priv = loadPrivateKey(process.env.ZERITH_UPDATE_SIGNING_KEY);
  const pub = crypto.createPublicKey(priv);
  console.log(rawPubkeyFromKeyObject(pub).toString("base64"));
}

function cmdSign(filePath, versionArg) {
  if (!filePath || !versionArg) {
    throw new Error("uso: sign <installer-path> <version>");
  }
  const version = normalizeVersion(versionArg);
  const fileName = path.basename(filePath);
  const priv = loadPrivateKey(process.env.ZERITH_UPDATE_SIGNING_KEY);
  const hex = sha256File(filePath);
  const payload = signingPayload(version, fileName, hex);
  const sig = crypto.sign(null, payload, priv);
  const sigB64 = sig.toString("base64");

  const shaPath = `${filePath}.sha256`;
  const sigPath = `${filePath}.sig`;
  fs.writeFileSync(shaPath, `${hex}  ${fileName}\n`, "utf8");
  fs.writeFileSync(sigPath, `${sigB64}\n`, "utf8");
  console.log(`sha256: ${shaPath}`);
  console.log(`sig:    ${sigPath}`);
}

function cmdVerify(filePath, versionArg, sigPath, pubArg) {
  if (!filePath || !versionArg || !sigPath) {
    throw new Error("uso: verify <installer-path> <version> <sig-path> [pubkey-b64-or-file]");
  }
  const version = normalizeVersion(versionArg);
  const fileName = path.basename(filePath);
  const rawPub = loadPubkey(pubArg);
  const pubKey = crypto.createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, rawPub]),
    format: "der",
    type: "spki",
  });
  const sigText = fs.readFileSync(sigPath, "utf8").trim();
  const sig = Buffer.from(sigText, "base64");
  if (sig.length !== 64) {
    throw new Error(`assinatura deve ter 64 bytes em base64 (recebido ${sig.length})`);
  }
  const hex = sha256File(filePath);
  const payload = signingPayload(version, fileName, hex);
  const ok = crypto.verify(null, payload, pubKey, sig);
  if (!ok) {
    throw new Error("assinatura Ed25519 INVÁLIDA");
  }
  console.log(`OK: assinatura Ed25519 válida (${fileName}, v${version}, sha256=${hex.slice(0, 12)}…)`);
}

try {
  const [cmd, ...args] = process.argv.slice(2);
  switch (cmd) {
    case "keygen":
      cmdKeygen();
      break;
    case "pubkey":
      cmdPubkey();
      break;
    case "sign":
      cmdSign(args[0], args[1]);
      break;
    case "verify":
      cmdVerify(args[0], args[1], args[2], args[3]);
      break;
    default:
      console.error(
        "uso: node scripts/sign-update.mjs <keygen|pubkey|sign|verify> ...",
      );
      process.exit(2);
  }
} catch (err) {
  console.error(`erro: ${err instanceof Error ? err.message : String(err)}`);
  process.exit(1);
}
