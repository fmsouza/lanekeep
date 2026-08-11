"""Extract a named node from a query match."""


def capture(m, name):
    """The node a capture name bound, or None when the capture did not participate.

    A match is a list of (name, node) entries; a capture that did not
    participate is simply absent. Returns None for a miss — compare with
    `is None`, never with truthiness, because the root's handle is 0 and 0 is
    falsy in Python.
    """
    for entry in m:
        if entry.name == name:
            return entry.node
    return None
