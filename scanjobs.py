"""扫描任务代数。后发起的扫描作废先发起、后返回的结果。"""


class ScanJobs:
    def __init__(self):
        self.current = {}

    def start(self, name: str) -> int:
        gen = self.current.get(name, 0) + 1
        self.current[name] = gen
        return gen

    def accept(self, name: str, gen: int) -> bool:
        return gen == self.current.get(name)
