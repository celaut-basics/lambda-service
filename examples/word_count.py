def handler(event):
    words = event["text"].split()
    counts = {}
    for w in words:
        counts[w] = counts.get(w, 0) + 1
    return {"words": len(words), "top": sorted(counts.items(), key=lambda kv: -kv[1])[:3]}
