# CPU and memory in the child: a sieve of Eratosthenes up to n.
# (MicroPython note: `bytearray(...) * n` and slice assignment with a step are not
# supported, so this uses a list and plain loops.)
def handler(event):
    n = event.get("n", 10000)
    sieve = [True] * (n + 1)
    sieve[0] = sieve[1] = False
    i = 2
    while i * i <= n:
        if sieve[i]:
            for j in range(i * i, n + 1, i):
                sieve[j] = False
        i += 1
    primes = [k for k in range(n + 1) if sieve[k]]
    return {"n": n, "count": len(primes), "last": primes[-1]}
