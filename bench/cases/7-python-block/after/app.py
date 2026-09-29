def handle(req):
    if req.ok:
        log(req)
        if req.slow:
            warn(req)
        return 200
    return 500
