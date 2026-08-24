"""streamaid entrypoint: argparse, wiring, signal handling."""

import argparse
import logging
import signal
import sys

from . import __version__
from .capture import Capture
from .config import load
from .hub import FrameHub
from .llm import Analyzer
from .server import StreamServer


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="streamaid", description="LAN screen-analysis assistant"
    )
    parser.add_argument(
        "-c", "--config", default="./config.json",
        help="config file path (default: ./config.json)",
    )
    parser.add_argument("--version", action="version", version=f"streamaid {__version__}")
    args = parser.parse_args(argv)

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    log = logging.getLogger("streamaid")

    try:
        cfg = load(args.config)
    except ValueError as e:
        log.error("config error: %s", e)
        return 2

    hub = FrameHub()
    capture = Capture()
    try:
        server = StreamServer((cfg.host, cfg.port), cfg, args.config, hub, capture)
    except OSError as e:
        log.error("cannot bind %s:%d: %s", cfg.host, cfg.port, e)
        return 1

    analyzer = Analyzer(cfg.llm, hub, on_analysis=server.publish_analysis)
    server.set_analyzer(analyzer)
    analyzer.start()

    capture.start(cfg, hub)

    log.info("streamaid %s listening on http://%s:%d", __version__, cfg.host, cfg.port)
    log.info("config file: %s", args.config)
    log.info("capture input: %s", capture.input)

    def _shutdown(signum, frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGINT, _shutdown)
    signal.signal(signal.SIGTERM, _shutdown)

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log.info("shutting down")
    finally:
        capture.stop()
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
