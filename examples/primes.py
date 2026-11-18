def handler(event):
    n = event.get("n", 10000)
    sieve = bytearray([1]) * (n + 1)
    sieve[0:2] = b"\x00\x00"
    for i in range(2, int(n ** 0.5) + 1):
        if sieve[i]:
            sieve[i * i::i] = bytearray(len(range(i * i, n + 1, i)))
    return {"n": n, "primes": sum(sieve), "last": max(i for i in range(n + 1) if sieve[i])}
