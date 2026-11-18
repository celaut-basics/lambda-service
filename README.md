# lambda-service

A Celaut service that runs a Python function in a child service, like AWS Lambda
(without its runtime API):

- the **parent** is written in Rust with
  [celaut-project/service-library](https://github.com/celaut-project/service-library);
- the **child** is only MicroPython: no server, no API, no HTTP client. It reads the
  function and its input from the directory that the parent shares with it (nodo's
  parent → child shared filesystem) and writes the result back there.

The implementation is in review.
