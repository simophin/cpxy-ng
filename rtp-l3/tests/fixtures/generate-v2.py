#!/usr/bin/env python3
"""Independent HKDF/HMAC + system libsodium oracle. Prints public test vectors."""
import ctypes
import ctypes.util
import hashlib
import hmac
import json
import sys

sodium = ctypes.CDLL(ctypes.util.find_library('sodium'))
sodium.sodium_init()

def material(secret, salt, label, length):
    prk = hmac.digest(salt, secret, 'sha256')
    output, block = b'', b''
    for index in range(1, (length + 31) // 32 + 1):
        block = hmac.digest(prk, block + label + bytes([index]), 'sha256')
        output += block
    return output[:length]

def encrypt(key, nonce, aad, body):
    output = ctypes.create_string_buffer(len(body) + 16)
    size = ctypes.c_ulonglong()
    assert sodium.crypto_aead_xchacha20poly1305_ietf_encrypt(
        output, ctypes.byref(size), body, ctypes.c_ulonglong(len(body)),
        aad, ctypes.c_ulonglong(len(aad)), None, nonce, key) == 0
    return output.raw[:size.value]

vectors = []
secret, allocation = bytes([7])*32, bytes([9])*16
ip = bytearray(range(60))
ip[0], ip[2:4], ip[12:16], ip[16:20] = 0x46, (60).to_bytes(2,'big'), bytes([10,0,0,2]), bytes([10,0,0,2])
header = bytes.fromhex('80600102030405060708090a0000000000000000')
for direction, offset in [('client-to-server',12), ('server-to-client',16)]:
    derived = material(secret, allocation, ('cpxy-rtp/v2/udp/'+direction).encode(),48)
    body = bytes(ip[:offset]+ip[offset+4:])
    encrypted = encrypt(derived[:32],derived[32:]+bytes(8),b'cpxy-rtp/v2/udp\0'+allocation+header,body)
    vectors.append(dict(direction=direction,key=derived[:32].hex(),prefix=derived[32:].hex(),ip=ip.hex(),frame=(header+encrypted).hex()))
if '--http' in sys.argv:
    identity=b'test'
    http_vectors=[]
    request_hash=None
    for kind,direction in [(1,'client-to-server'),(2,'server-to-client')]:
        nonce=bytes(range(0x40,0x58))
        key=material(secret,identity,('cpxy-rtp/v2/http/'+direction).encode(),32)
        value=dict(token='01'*16)
        if kind==2:
            value.update(request_hash=request_hash,generation='02'*16,challenge='03'*16,valid_for_ms=30000,server_age_ms=1000)
        plaintext=json.dumps(value,separators=(',',':'),sort_keys=True).encode()
        aad=b'cpxy-rtp/v2/http\0'+bytes([len(identity)])+identity+bytes([2,kind])
        body=bytes([2,kind])+nonce+encrypt(key,nonce,aad,plaintext)
        http_vectors.append(dict(kind=kind,plaintext=plaintext.decode(),body=body.hex()))
        if kind==1:
            request_hash=hashlib.sha256(body).hexdigest()
    print(json.dumps(http_vectors,indent=2))
else:
    print(json.dumps(vectors,indent=2))
