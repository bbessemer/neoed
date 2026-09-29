def handle(req):
    if req.ok:
        log(req)
        return 200
    return 500
