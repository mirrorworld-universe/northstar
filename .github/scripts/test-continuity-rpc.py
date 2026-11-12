#!/usr/bin/env python3
"""Bound public read pacing and rate-limit recovery without replaying writes."""
import io
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

import continuity_devnet as rollout


class RpcReadTests(unittest.TestCase):
    def setUp(self):
        self.now = 100.0
        self.sleeps = []
        def sleep(delay):
            self.sleeps.append(delay)
            self.now += delay
        clock = patch.object(rollout.time, "monotonic", side_effect=lambda: self.now)
        sleeper = patch.object(rollout.time, "sleep", side_effect=sleep)
        clock.start()
        sleeper.start()
        self.addCleanup(clock.stop)
        self.addCleanup(sleeper.stop)
        self.rpc = rollout.Rpc("https://api.devnet.solana.com")

    def response(self):
        return io.BytesIO(b'{"jsonrpc":"2.0","id":1,"result":123}')

    def limited(self, retry_after=None, code=429):
        headers = {} if retry_after is None else {"Retry-After": retry_after}
        return HTTPError("https://private.invalid/token", code, "private response", headers, None)

    def opener(self, responses):
        self.rpc.opener = SimpleNamespace(open=Mock(side_effect=responses))
        return self.rpc.opener.open

    def test_public_read_recovers_with_bounded_backoff_and_identical_request(self):
        opened = self.opener([self.limited(), self.response()])
        self.assertEqual(self.rpc.call("getSlot", [{"commitment": "finalized"}]), 123)
        self.assertEqual(opened.call_count, 2)
        self.assertEqual(self.sleeps, [5.0])
        self.assertEqual(opened.call_args_list[0].args[0].data, opened.call_args_list[1].args[0].data)

    def test_retry_after_is_respected_but_excessive_wait_is_not_shortened(self):
        opened = self.opener([self.limited("10"), self.response()])
        self.assertEqual(self.rpc.call("getSlot"), 123)
        self.assertEqual(self.sleeps, [10.0])
        opened = self.opener([self.limited("60")])
        with self.assertRaises(HTTPError):
            self.rpc.call("getSlot")
        self.assertEqual(opened.call_count, 1)
        self.assertNotIn(30.0, self.sleeps)

    def test_write_or_unrelated_http_error_is_never_retried(self):
        for method, code in (("sendTransaction", 429), ("getSlot", 403)):
            opened = self.opener([self.limited(code=code)])
            with self.assertRaises(HTTPError):
                self.rpc.call(method, ["unchanged"])
            self.assertEqual(opened.call_count, 1)

    def test_public_read_attempt_budget_is_finite(self):
        opened = self.opener([self.limited() for _ in range(4)])
        with self.assertRaises(HTTPError):
            self.rpc.call("getSlot")
        self.assertEqual(opened.call_count, 4)
        self.assertEqual(self.sleeps, [5.0, 10.0, 20.0])

    def test_public_reads_are_paced_and_loopback_reads_are_not(self):
        self.opener([self.response(), self.response()])
        self.rpc.call("getSlot")
        self.rpc.call("getGenesisHash")
        self.assertEqual(self.sleeps, [1.0])
        self.sleeps.clear()
        self.rpc = rollout.Rpc("http://127.0.0.1:8899")
        self.opener([self.response(), self.response()])
        self.rpc.call("getSlot")
        self.rpc.call("getGenesisHash")
        self.assertEqual(self.sleeps, [])

    def test_retry_delay_cannot_exceed_the_read_deadline(self):
        def late_error(*_, **__):
            self.now += 44.0
            raise self.limited("2")
        opened = self.opener(late_error)
        with self.assertRaises(HTTPError):
            self.rpc.call("getSlot")
        self.assertEqual(opened.call_count, 1)
        self.assertEqual(self.sleeps, [])


if __name__ == "__main__":
    unittest.main()
