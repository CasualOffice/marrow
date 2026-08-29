"""A fixture, not real code."""

import json


@staticmethod
def decorated():
    return 1


def load(text):
    return json.loads(text)


class Router:
    def __init__(self, parsers):
        self.parsers = parsers

    def parse(self, data):
        for parser in self.parsers:
            if parser.handles(data):
                return parser.parse(data)
        return None
