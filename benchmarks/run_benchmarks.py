#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
beetfs Benchmark Suite
Measures mount time, metadata ops, file I/O, and memory usage.
"""
from __future__ import print_function
import os
import sys
import time
import json
import tempfile
import shutil
import subprocess
import signal
import resource
import datetime

# Add project paths
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'beetsplug'))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tests'))

from conftest import create_synthetic_flac


class BenchmarkResult(object):
    """Stores benchmark results."""
    def __init__(self, name):
        self.name = name
        self.timings = []
        self.memory_kb = None
        self.error = None
        self.metadata = {}

    def add_timing(self, seconds):
        self.timings.append(seconds)

    @property
    def mean(self):
        if not self.timings:
            return None
        return sum(self.timings) / len(self.timings)

    @property
    def min_time(self):
        return min(self.timings) if self.timings else None

    @property
    def max_time(self):
        return max(self.timings) if self.timings else None

    def to_dict(self):
        return {
            'name': self.name,
            'mean_ms': self.mean * 1000 if self.mean else None,
            'min_ms': self.min_time * 1000 if self.min_time else None,
            'max_ms': self.max_time * 1000 if self.max_time else None,
            'runs': len(self.timings),
            'memory_kb': self.memory_kb,
            'error': self.error,
            'metadata': self.metadata
        }


class BeetFSBenchmark(object):
    """Benchmark harness for beetfs."""

    def __init__(self, output_dir):
        self.output_dir = output_dir
        self.results = []
        self.temp_dir = None
        self.mount_dir = None
        self.music_dir = None
        self.db_path = None
        self.mount_process = None

    def setup(self, num_tracks=10, track_size_mb=5):
        """Create test environment with synthetic tracks."""
        self.temp_dir = tempfile.mkdtemp(prefix='beetfs_bench_')
        self.mount_dir = os.path.join(self.temp_dir, 'mount')
        self.music_dir = os.path.join(self.temp_dir, 'music')
        self.db_path = os.path.join(self.temp_dir, 'library.db')
        self.config_dir = os.path.join(self.temp_dir, 'config')

        os.makedirs(self.mount_dir)
        os.makedirs(self.music_dir)
        os.makedirs(self.config_dir)

        # Create beets config
        config_path = os.path.join(self.config_dir, 'config.yaml')
        with open(config_path, 'w') as f:
            f.write('directory: {}\n'.format(self.music_dir))
            f.write('library: {}\n'.format(self.db_path))
            f.write('plugins: []\n')

        os.environ['BEETSDIR'] = self.config_dir

        # Create synthetic FLAC files
        print("Creating {} synthetic tracks ({} MB each)...".format(num_tracks, track_size_mb))
        track_paths = []
        for i in range(num_tracks):
            artist = 'Bench Artist'
            album = 'Bench Album'
            title = 'Track {:03d}'.format(i + 1)
            filename = '{:02d} - {} - {}.flac'.format(i + 1, artist, title)
            track_path = os.path.join(self.music_dir, artist, album, filename)
            self._makedirs(os.path.dirname(track_path))
            create_synthetic_flac(track_path, duration_sec=track_size_mb * 10,
                                  artist=artist, title=title, album=album, track=str(i + 1))
            track_paths.append(track_path)

        # Import into beets library
        print("Importing tracks into beets library...")
        from beets import config
        from beets.library import Library
        config.read(user=False)
        config['directory'].set(self.music_dir)
        config['library'].set(self.db_path)

        lib = Library(self.db_path)
        from beets.library import Item
        for i, path in enumerate(track_paths):
            item = Item(
                path=path,
                artist=u'Bench Artist',
                album=u'Bench Album',
                title=u'Track {:03d}'.format(i + 1),
                track=i + 1,
                year=2024,
                genre=u'Benchmark',
                format='flac'
            )
            lib.add(item)
        lib._close()

        return len(track_paths)

    def _makedirs(self, path):
        """Python 2 compatible makedirs."""
        if not os.path.exists(path):
            os.makedirs(path)

    def teardown(self):
        """Clean up test environment."""
        self.unmount()
        if self.temp_dir and os.path.exists(self.temp_dir):
            shutil.rmtree(self.temp_dir, ignore_errors=True)

    def mount(self):
        """Mount beetfs and return time taken."""
        # Create mount script
        beetfs_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        beetsplug = os.path.join(beetfs_root, 'beetsplug')

        mount_script = '''
import sys
sys.path.insert(0, "{beetfs_root}")
sys.path.insert(0, "{beetsplug}")

import os
import re
os.environ["BEETSDIR"] = "{config_dir}"

from beets import config
from beets.library import Library

config.read(user=False)
config["directory"] = "{music_dir}"
config["library"] = "{db_path}"

lib = Library("{db_path}")

import beetFs
import fuse

fuse.fuse_python_api = (0, 2)

beetFs.library = lib
beetFs.structure_depth = 4
beetFs.structure_split = [0, 1, 2, 3]
beetFs.directory_structure = beetFs.FSNode({{}}, {{}})

for item in lib.items():
    mapping = beetFs.template_mapping(lib, item)
    path_str = beetFs.PATH_FORMAT
    for key, val in mapping.items():
        if val is not None:
            clean_val = re.sub(r"[\\\\/:]|^\\.", "_", unicode(val))
            path_str = path_str.replace("$" + key, clean_val)
    elements = path_str.split("/")
    sub_elements = elements[0:beetFs.structure_depth-1]
    for level in range(len(sub_elements)):
        level_subbed = sub_elements[0:level+1]
        beetFs.directory_structure.adddir(sub_elements, level_subbed[level])
    beetFs.directory_structure.addfile(
        sub_elements,
        elements[beetFs.structure_depth-1],
        item.id
    )

fs = beetFs.beetFileSystem(
    version="%prog " + fuse.__version__,
    usage="beetfs benchmark",
    dash_s_do="setsingle"
)

fs.parser.add_option(mountopt="root", metavar="PATH", default="{music_dir}",
                     help="music library root path")
fs.parse(args=["{mount_dir}"], errex=1)
fs.flags = 0
fs.multithreaded = False
fs.fuse_args.setmod("foreground")
fs.fuse_args.add("fsname=beetfs")
fs.fuse_args.add("nonempty")
fs.lib = lib

fs.main()
'''.format(
            beetfs_root=beetfs_root,
            beetsplug=beetsplug,
            config_dir=self.config_dir,
            music_dir=self.music_dir,
            db_path=self.db_path,
            mount_dir=self.mount_dir
        )

        script_path = os.path.join(self.temp_dir, 'mount.py')
        with open(script_path, 'w') as f:
            f.write(mount_script)

        start_time = time.time()
        self.mount_process = subprocess.Popen(
            [sys.executable, script_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE
        )

        # Wait for mount
        timeout = 30
        poll_interval = 0.05
        elapsed = 0
        while elapsed < timeout:
            if os.path.ismount(self.mount_dir):
                mount_time = time.time() - start_time
                return mount_time
            time.sleep(poll_interval)
            elapsed += poll_interval

            # Check if process died
            if self.mount_process.poll() is not None:
                stdout, stderr = self.mount_process.communicate()
                raise RuntimeError("Mount process died: {}".format(stderr.decode('utf-8', errors='replace')))

        raise RuntimeError("Mount timeout after {} seconds".format(timeout))

    def unmount(self):
        """Unmount beetfs."""
        if os.path.ismount(self.mount_dir):
            subprocess.call(['fusermount', '-u', self.mount_dir])
            time.sleep(0.5)

        if self.mount_process and self.mount_process.poll() is None:
            self.mount_process.terminate()
            try:
                self.mount_process.wait(timeout=5)
            except:
                self.mount_process.kill()

    def get_memory_usage(self):
        """Get current process memory usage in KB."""
        if self.mount_process and self.mount_process.poll() is None:
            try:
                with open('/proc/{}/status'.format(self.mount_process.pid)) as f:
                    for line in f:
                        if line.startswith('VmRSS:'):
                            return int(line.split()[1])
            except:
                pass
        return None

    # ========================
    # BENCHMARK METHODS
    # ========================

    def bench_mount_time(self, runs=5):
        """Benchmark mount time."""
        result = BenchmarkResult('mount_time')
        print("\n=== Mount Time Benchmark ({} runs) ===".format(runs))

        for i in range(runs):
            try:
                mount_time = self.mount()
                result.add_timing(mount_time)
                print("  Run {}: {:.3f}s".format(i + 1, mount_time))
                result.memory_kb = self.get_memory_usage()
                self.unmount()
                time.sleep(0.5)
            except Exception as e:
                result.error = str(e)
                print("  Run {}: ERROR - {}".format(i + 1, e))
                break

        self.results.append(result)
        return result

    def bench_stat_latency(self, runs=50):
        """Benchmark single stat() call latency."""
        result = BenchmarkResult('stat_latency')
        print("\n=== Stat Latency Benchmark ({} runs) ===".format(runs))

        try:
            self.mount()
            time.sleep(1)  # Let mount settle

            # Find a file to stat
            test_path = None
            for root, dirs, files in os.walk(self.mount_dir):
                if files:
                    test_path = os.path.join(root, files[0])
                    break

            if not test_path:
                result.error = "No files found in mount"
                self.results.append(result)
                return result

            result.metadata['test_path'] = test_path

            for i in range(runs):
                start = time.time()
                try:
                    os.stat(test_path)
                    elapsed = time.time() - start
                    result.add_timing(elapsed)
                except OSError as e:
                    result.error = "stat failed: {} (errno {})".format(e.strerror, e.errno)
                    print("  ERROR: {}".format(result.error))
                    break

            if result.timings:
                print("  Mean: {:.3f}ms, Min: {:.3f}ms, Max: {:.3f}ms".format(
                    result.mean * 1000, result.min_time * 1000, result.max_time * 1000))

            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def bench_readdir(self, runs=20):
        """Benchmark directory listing."""
        result = BenchmarkResult('readdir')
        print("\n=== Readdir Benchmark ({} runs) ===".format(runs))

        try:
            self.mount()
            time.sleep(1)

            for i in range(runs):
                start = time.time()
                try:
                    entries = os.listdir(self.mount_dir)
                    elapsed = time.time() - start
                    result.add_timing(elapsed)
                    if i == 0:
                        result.metadata['entry_count'] = len(entries)
                except OSError as e:
                    result.error = "listdir failed: {} (errno {})".format(e.strerror, e.errno)
                    print("  ERROR: {}".format(result.error))
                    break

            if result.timings:
                print("  Mean: {:.3f}ms, Entries: {}".format(
                    result.mean * 1000, result.metadata.get('entry_count', 'N/A')))

            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def bench_file_open(self, runs=10):
        """Benchmark file open latency."""
        result = BenchmarkResult('file_open')
        print("\n=== File Open Benchmark ({} runs) ===".format(runs))

        try:
            self.mount()
            time.sleep(1)

            # Find a file to open
            test_path = None
            for root, dirs, files in os.walk(self.mount_dir):
                if files:
                    test_path = os.path.join(root, files[0])
                    break

            if not test_path:
                result.error = "No files found in mount"
                self.results.append(result)
                return result

            result.metadata['test_path'] = test_path

            for i in range(runs):
                # Clear page cache between runs (requires sudo, skip if not available)
                try:
                    subprocess.call(['sync'])
                except:
                    pass

                start = time.time()
                try:
                    f = open(test_path, 'rb')
                    f.read(1)  # Trigger actual open
                    f.close()
                    elapsed = time.time() - start
                    result.add_timing(elapsed)
                except (IOError, OSError) as e:
                    result.error = "open failed: {}".format(e)
                    print("  ERROR: {}".format(result.error))
                    break

            if result.timings:
                print("  Mean: {:.3f}ms, Min: {:.3f}ms, Max: {:.3f}ms".format(
                    result.mean * 1000, result.min_time * 1000, result.max_time * 1000))

            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def bench_read_throughput(self):
        """Benchmark read throughput."""
        result = BenchmarkResult('read_throughput')
        print("\n=== Read Throughput Benchmark ===")

        try:
            self.mount()
            time.sleep(1)

            # Find a file to read
            test_path = None
            for root, dirs, files in os.walk(self.mount_dir):
                if files:
                    test_path = os.path.join(root, files[0])
                    break

            if not test_path:
                result.error = "No files found in mount"
                self.results.append(result)
                return result

            result.metadata['test_path'] = test_path

            # Read entire file and measure throughput
            start = time.time()
            try:
                with open(test_path, 'rb') as f:
                    data = f.read()
                elapsed = time.time() - start

                file_size = len(data)
                throughput_mbps = (file_size / (1024 * 1024)) / elapsed if elapsed > 0 else 0

                result.add_timing(elapsed)
                result.metadata['file_size_bytes'] = file_size
                result.metadata['throughput_mbps'] = throughput_mbps

                print("  File size: {:.2f} MB, Time: {:.3f}s, Throughput: {:.2f} MB/s".format(
                    file_size / (1024 * 1024), elapsed, throughput_mbps))

            except (IOError, OSError) as e:
                result.error = "read failed: {}".format(e)
                print("  ERROR: {}".format(result.error))

            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def bench_memory_usage(self):
        """Benchmark memory usage."""
        result = BenchmarkResult('memory_usage')
        print("\n=== Memory Usage Benchmark ===")

        try:
            self.mount()
            time.sleep(2)

            # Measure idle memory
            idle_mem = self.get_memory_usage()
            result.metadata['idle_memory_kb'] = idle_mem
            print("  Idle memory: {} KB".format(idle_mem))

            # Open a file and measure
            test_path = None
            for root, dirs, files in os.walk(self.mount_dir):
                if files:
                    test_path = os.path.join(root, files[0])
                    break

            if test_path:
                try:
                    with open(test_path, 'rb') as f:
                        f.read()
                    after_read_mem = self.get_memory_usage()
                    result.metadata['after_read_memory_kb'] = after_read_mem
                    print("  After file read: {} KB".format(after_read_mem))
                    if idle_mem and after_read_mem:
                        print("  Memory increase: {} KB".format(after_read_mem - idle_mem))
                except (IOError, OSError) as e:
                    result.error = "read failed: {}".format(e)

            result.memory_kb = self.get_memory_usage()
            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def bench_enoent_lookup(self, runs=50):
        """Benchmark ENOENT lookup (missing file) latency."""
        result = BenchmarkResult('enoent_lookup')
        print("\n=== ENOENT Lookup Benchmark ({} runs) ===".format(runs))

        try:
            self.mount()
            time.sleep(1)

            # Non-existent file path
            missing_path = os.path.join(self.mount_dir, 'nonexistent', 'cover.jpg')

            for i in range(runs):
                start = time.time()
                try:
                    os.stat(missing_path)
                except OSError:
                    pass  # Expected
                elapsed = time.time() - start
                result.add_timing(elapsed)

            if result.timings:
                print("  Mean: {:.3f}ms, Min: {:.3f}ms, Max: {:.3f}ms".format(
                    result.mean * 1000, result.min_time * 1000, result.max_time * 1000))

            self.unmount()
        except Exception as e:
            result.error = str(e)
            print("  ERROR: {}".format(e))

        self.results.append(result)
        return result

    def save_results(self, filename='benchmark_results.json'):
        """Save results to JSON file."""
        output_path = os.path.join(self.output_dir, filename)
        data = {
            'timestamp': datetime.datetime.now().isoformat(),
            'results': [r.to_dict() for r in self.results]
        }
        with open(output_path, 'w') as f:
            json.dump(data, f, indent=2)
        print("\nResults saved to: {}".format(output_path))
        return output_path


def main():
    print("=" * 60)
    print("beetfs Benchmark Suite")
    print("=" * 60)

    output_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'results')
    if not os.path.exists(output_dir):
        os.makedirs(output_dir)

    bench = BeetFSBenchmark(output_dir)

    try:
        # Setup with 10 tracks, 5MB each
        num_tracks = bench.setup(num_tracks=10, track_size_mb=5)
        print("Setup complete: {} tracks".format(num_tracks))

        # Run benchmarks
        bench.bench_mount_time(runs=3)
        bench.bench_readdir(runs=10)
        bench.bench_stat_latency(runs=20)
        bench.bench_enoent_lookup(runs=20)
        bench.bench_file_open(runs=5)
        bench.bench_read_throughput()
        bench.bench_memory_usage()

        # Save results
        bench.save_results()

    finally:
        bench.teardown()

    # Print summary
    print("\n" + "=" * 60)
    print("SUMMARY")
    print("=" * 60)
    for r in bench.results:
        status = "OK" if not r.error else "FAIL"
        mean_str = "{:.3f}ms".format(r.mean * 1000) if r.mean else "N/A"
        print("{:20} {:6} Mean: {:>12} Error: {}".format(
            r.name, status, mean_str, r.error or "None"))


if __name__ == '__main__':
    main()
