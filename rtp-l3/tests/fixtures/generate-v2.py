#!/usr/bin/env python3
"""Independent HKDF/HMAC + system libsodium oracle. Prints public test vectors."""
import ctypes
import ctypes.util
import hashlib
import hmac
import json

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
print(json.dumps(vectors,indent=2))
