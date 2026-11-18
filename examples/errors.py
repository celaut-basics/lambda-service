# An exception in the function comes back as "error" and "traceback".
def handler(event):
    return 1 / event["divisor"]
