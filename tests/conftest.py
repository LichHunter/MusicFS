# -*- coding: utf-8 -*-
"""
beetfs E2E Test Infrastructure

Base test class, fixtures, and utilities for testing the real beetfs FUSE filesystem.
Python 2.7 compatible - no mocks, real filesystem operations.
"""
from __future__ import print_function, unicode_literals

import unittest
import subprocess
import tempfile
import shutil
import os
import sys
import time
import threading
import sqlite3

# Add parent directory to path for imports
BEETFS_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, BEETFS_ROOT)
sys.path.insert(0, os.path.join(BEETFS_ROOT, 'beetsplug'))

# Test constants
MOUNT_TIMEOUT = 30  # seconds
TEARDOWN_TIMEOUT = 5  # seconds
SYNTHETIC_FLAC_DURATION = 3  # seconds (smaller = faster tests)

# Real music files location (from qBittorrent container)
REAL_MUSIC_PATH = '/home/fujin/.local/share/docker/volumes/containers_downloads/_data/Metallica - 72 Seasons (2023) [FLAC] 88'


def is_fuse_available():
    """Check if FUSE is available on the system"""
    try:
        with open(os.devnull, 'w') as devnull:
            subprocess.check_call(['which', 'fusermount'],
                                  stdout=devnull, stderr=devnull)
        return True
    except (subprocess.CalledProcessError, OSError):
        return False


def is_ffmpeg_available():
    """Check if ffmpeg is available for synthetic FLAC generation"""
    try:
        with open(os.devnull, 'w') as devnull:
            subprocess.check_call(['which', 'ffmpeg'],
                                  stdout=devnull, stderr=devnull)
        return True
    except (subprocess.CalledProcessError, OSError):
        return False


def is_flac_available():
    """Check if flac encoder is available"""
    try:
        with open(os.devnull, 'w') as devnull:
            subprocess.check_call(['which', 'flac'],
                                  stdout=devnull, stderr=devnull)
        return True
    except (subprocess.CalledProcessError, OSError):
        return False


def create_synthetic_flac(output_path, duration_sec=SYNTHETIC_FLAC_DURATION,
                          artist='Test Artist', title='Test Track',
                          album='Test Album', year='2024', track='01',
                          genre='Test Genre'):
    """
    Create a minimal FLAC file with known metadata.
    
    Uses ffmpeg to generate silence, then flac to encode with tags.
    Result is ~100-500KB depending on duration.
    
    Args:
        output_path: Where to write the FLAC file
        duration_sec: Duration in seconds (default 3)
        artist, title, album, year, track, genre: Metadata tags
    
    Returns:
        Path to created FLAC file
    
    Raises:
        subprocess.CalledProcessError: If ffmpeg or flac fails
        RuntimeError: If tools not available
    """
    if not is_ffmpeg_available():
        raise RuntimeError("ffmpeg not available - install it or use nix develop")
    if not is_flac_available():
        raise RuntimeError("flac not available - install it or use nix develop")
    
    # Create temp WAV file
    wav_fd, wav_path = tempfile.mkstemp(suffix='.wav')
    os.close(wav_fd)
    
    try:
        # Generate silence WAV using ffmpeg
        with open(os.devnull, 'w') as devnull:
            subprocess.check_call([
                'ffmpeg', '-f', 'lavfi', '-i',
                'anullsrc=r=44100:cl=stereo',
                '-t', str(duration_sec),
                '-y', wav_path
            ], stdout=devnull, stderr=devnull)
        
        # Convert to FLAC with metadata tags
        with open(os.devnull, 'w') as devnull:
            subprocess.check_call([
                'flac', '--best', '--silent',
                '-T', 'ARTIST={}'.format(artist),
                '-T', 'TITLE={}'.format(title),
                '-T', 'ALBUM={}'.format(album),
                '-T', 'DATE={}'.format(year),
                '-T', 'TRACKNUMBER={}'.format(track),
                '-T', 'GENRE={}'.format(genre),
                '-o', output_path,
                wav_path
            ], stdout=devnull, stderr=devnull)
        
        return output_path
    finally:
        # Cleanup temp WAV
        if os.path.exists(wav_path):
            os.unlink(wav_path)


class BeetsLibraryFixture(object):
    """
    Creates an isolated beets library for testing.
    
    Sets up:
    - Temp directory for library files
    - SQLite database with test items
    - Config pointing to the test library
    """
    
    def __init__(self, temp_dir=None):
        self.temp_dir = temp_dir or tempfile.mkdtemp(prefix='beetfs_lib_')
        self.db_path = os.path.join(self.temp_dir, 'library.db')
        self.music_dir = os.path.join(self.temp_dir, 'music')
        self.config_path = os.path.join(self.temp_dir, 'config.yaml')
        
        os.makedirs(self.music_dir)
        self._create_config()
        self._create_database()
    
    def _create_config(self):
        """Create minimal beets config"""
        config_content = """
directory: {music_dir}
library: {db_path}
import:
    copy: no
    move: no
    write: no
plugins: []
""".format(music_dir=self.music_dir, db_path=self.db_path)
        
        with open(self.config_path, 'w') as f:
            f.write(config_content)
    
    def _create_database(self):
        """Create empty beets SQLite database with correct schema"""
        conn = sqlite3.connect(self.db_path)
        cursor = conn.cursor()
        
        # Create items table (simplified schema matching beets)
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY,
                path BLOB NOT NULL,
                album_id INTEGER,
                title TEXT,
                artist TEXT,
                album TEXT,
                genre TEXT,
                year INTEGER,
                track INTEGER,
                disc INTEGER,
                length REAL,
                bitrate INTEGER,
                format TEXT,
                samplerate INTEGER,
                bitdepth INTEGER,
                channels INTEGER,
                mtime REAL,
                added REAL
            )
        ''')
        
        # Create albums table
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS albums (
                id INTEGER PRIMARY KEY,
                artpath BLOB,
                added REAL,
                albumartist TEXT,
                album TEXT,
                genre TEXT,
                year INTEGER
            )
        ''')
        
        conn.commit()
        conn.close()
    
    def add_item(self, path, title='Test Track', artist='Test Artist',
                 album='Test Album', genre='Test Genre', year=2024,
                 track=1, format='flac'):
        """
        Add an item to the test library database.
        
        Args:
            path: Absolute path to the audio file
            title, artist, album, genre, year, track: Metadata
            format: Audio format (flac, mp3, etc.)
        
        Returns:
            Item ID
        """
        conn = sqlite3.connect(self.db_path)
        cursor = conn.cursor()
        
        cursor.execute('''
            INSERT INTO items (path, title, artist, album, genre, year, track, format, mtime, added)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ''', (path, title, artist, album, genre, year, track, format,
              time.time(), time.time()))
        
        item_id = cursor.lastrowid
        conn.commit()
        conn.close()
        
        return item_id
    
    def get_item(self, item_id):
        """Get item by ID"""
        conn = sqlite3.connect(self.db_path)
        cursor = conn.cursor()
        cursor.execute('SELECT * FROM items WHERE id = ?', (item_id,))
        row = cursor.fetchone()
        conn.close()
        return row
    
    def update_item(self, item_id, **kwargs):
        """Update item metadata"""
        conn = sqlite3.connect(self.db_path)
        cursor = conn.cursor()
        
        set_clause = ', '.join('{} = ?'.format(k) for k in kwargs.keys())
        values = list(kwargs.values()) + [item_id]
        
        cursor.execute(
            'UPDATE items SET {} WHERE id = ?'.format(set_clause),
            values
        )
        
        conn.commit()
        conn.close()
    
    def cleanup(self):
        """Remove all temp files"""
        if os.path.exists(self.temp_dir):
            shutil.rmtree(self.temp_dir, ignore_errors=True)


class BeetFSTestCase(unittest.TestCase):
    """
    Base test class for beetfs e2e tests.
    
    Provides:
    - FUSE availability checking
    - Mount/unmount helpers with timeout
    - Beets library fixture management
    - Cleanup on test failure
    
    Python 2.7 compatible (no subprocess.timeout, etc.)
    """
    
    @classmethod
    def setUpClass(cls):
        """Check FUSE availability once for all tests"""
        if not is_fuse_available():
            raise unittest.SkipTest("fusermount not available - FUSE required for e2e tests")
    
    def setUp(self):
        """Create fresh temp directory and library for each test"""
        self.temp_dir = tempfile.mkdtemp(prefix='beetfs_test_')
        self.mount_dir = os.path.join(self.temp_dir, 'mount')
        os.makedirs(self.mount_dir)
        
        self.library = BeetsLibraryFixture(
            temp_dir=os.path.join(self.temp_dir, 'library')
        )
        
        self.fs_process = None
        self._timeout_timer = None
    
    def tearDown(self):
        """Cleanup: unmount filesystem and remove temp files"""
        # Cancel any pending timeout
        if self._timeout_timer:
            self._timeout_timer.cancel()
        
        # Unmount if mounted
        if self.fs_process and os.path.ismount(self.mount_dir):
            self._unmount()
        
        # Kill process if still running
        if self.fs_process and self.fs_process.poll() is None:
            self.fs_process.terminate()
            self._wait_for_process_exit(TEARDOWN_TIMEOUT)
            if self.fs_process.poll() is None:
                self.fs_process.kill()
        
        # Cleanup library
        if hasattr(self, 'library'):
            self.library.cleanup()
        
        # Remove temp directory
        if hasattr(self, 'temp_dir') and os.path.exists(self.temp_dir):
            shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    def mount_beetfs(self, foreground=True):
        """
        Mount beetfs filesystem.
        
        Args:
            foreground: Run in foreground mode (required for testing)
        
        Raises:
            AssertionError: If mount fails or times out
        """
        # Build command to run beetfs
        # We need to invoke beets with our test config and library
        env = os.environ.copy()
        env['BEETSDIR'] = self.library.temp_dir
        
        cmd = [
            sys.executable,  # Python interpreter
            '-c',
            '''
import sys
sys.path.insert(0, "{beetfs_root}")
sys.path.insert(0, "{beetsplug}")

import os
os.environ["BEETSDIR"] = "{beetsdir}"

from beets import config
from beets.library import Library

# Load our test config
config.read(user=False)
config["directory"] = "{music_dir}"
config["library"] = "{db_path}"

# Open library
lib = Library("{db_path}")

# Import and run beetfs
from beetFs import beetFileSystem
import fuse

fuse.fuse_python_api = (0, 2)
fs = beetFileSystem(
    version="%prog " + fuse.__version__,
    usage="Test mount",
    dash_s_do='setsingle'
)
fs.parse(errex=1)
fs.flags = 0
fs.multithreaded = False

# Set global library reference
import beetFs
beetFs.library = lib

# Build directory structure
from beetFs import directory_structure, template_mapping
for item in lib.items():
    mapping = beetFs.template_mapping(lib, item)
    
fs.main()
'''.format(
                beetfs_root=BEETFS_ROOT,
                beetsplug=os.path.join(BEETFS_ROOT, 'beetsplug'),
                beetsdir=self.library.temp_dir,
                music_dir=self.library.music_dir,
                db_path=self.library.db_path
            ),
            self.mount_dir
        ]
        
        if foreground:
            cmd.insert(-1, '-f')
        
        # Start process
        self.fs_process = subprocess.Popen(
            cmd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE
        )
        
        # Setup timeout using threading.Timer (Python 2.7 compatible)
        self._timeout_timer = threading.Timer(
            MOUNT_TIMEOUT,
            self._mount_timeout_handler
        )
        self._timeout_timer.start()
        
        # Wait for mount
        try:
            self._wait_for_mount()
        finally:
            self._timeout_timer.cancel()
    
    def _mount_timeout_handler(self):
        """Called if mount times out"""
        if self.fs_process and self.fs_process.poll() is None:
            self.fs_process.kill()
    
    def _wait_for_mount(self):
        """Poll until filesystem is mounted"""
        start = time.time()
        while time.time() - start < MOUNT_TIMEOUT:
            if os.path.ismount(self.mount_dir):
                return
            
            # Check if process died
            if self.fs_process.poll() is not None:
                stdout, stderr = self.fs_process.communicate()
                self.fail(
                    "beetfs process terminated prematurely with code {}.\n"
                    "stdout: {}\nstderr: {}".format(
                        self.fs_process.returncode,
                        stdout.decode('utf-8', errors='replace'),
                        stderr.decode('utf-8', errors='replace')
                    )
                )
            
            time.sleep(0.1)
        
        self.fail("Mount timeout after {} seconds".format(MOUNT_TIMEOUT))
    
    def _unmount(self):
        """Unmount the filesystem using fusermount"""
        with open(os.devnull, 'w') as devnull:
            # Use -z for lazy unmount (handles busy mounts)
            subprocess.call(
                ['fusermount', '-z', '-u', self.mount_dir],
                stdout=devnull,
                stderr=devnull
            )
    
    def _wait_for_process_exit(self, timeout):
        """Wait for process to exit (Python 2.7 compatible)"""
        start = time.time()
        while time.time() - start < timeout:
            if self.fs_process.poll() is not None:
                return True
            time.sleep(0.1)
        return False
    
    def create_test_flac(self, filename='test.flac', **metadata):
        """
        Create a synthetic FLAC in the library music directory.
        
        Args:
            filename: Name for the file
            **metadata: Passed to create_synthetic_flac()
        
        Returns:
            Absolute path to created file
        """
        output_path = os.path.join(self.library.music_dir, filename)
        create_synthetic_flac(output_path, **metadata)
        return output_path
    
    def add_test_track(self, filename='test.flac', **metadata):
        """
        Create a synthetic FLAC and add it to the library.
        
        Args:
            filename: Name for the file
            **metadata: Metadata for both file and database
        
        Returns:
            Tuple of (file_path, item_id)
        """
        # Default metadata
        defaults = {
            'artist': 'Test Artist',
            'title': 'Test Track',
            'album': 'Test Album',
            'genre': 'Test Genre',
            'year': '2024',
            'track': '01'
        }
        defaults.update(metadata)
        
        # Create the file
        file_path = self.create_test_flac(filename, **defaults)
        
        # Add to database (convert year and track to int for DB)
        item_id = self.library.add_item(
            path=file_path,
            title=defaults['title'],
            artist=defaults['artist'],
            album=defaults['album'],
            genre=defaults['genre'],
            year=int(defaults['year']),
            track=int(defaults['track']),
            format='flac'
        )
        
        return file_path, item_id


class RealMusicTestCase(BeetFSTestCase):
    """
    Test case that uses real music files from qBittorrent.
    
    Skipped if real music not available or E2E env var not set.
    """
    
    @classmethod
    def setUpClass(cls):
        super(RealMusicTestCase, cls).setUpClass()
        
        # Check if E2E tests are enabled
        if not os.environ.get('E2E'):
            raise unittest.SkipTest("E2E tests disabled - set E2E=1 to enable")
        
        # Check if real music is available
        if not os.path.isdir(REAL_MUSIC_PATH):
            raise unittest.SkipTest(
                "Real music not available at {}".format(REAL_MUSIC_PATH)
            )
    
    def get_real_flac_files(self):
        """Get list of real FLAC files"""
        files = []
        for f in os.listdir(REAL_MUSIC_PATH):
            if f.endswith('.flac'):
                files.append(os.path.join(REAL_MUSIC_PATH, f))
        return sorted(files)


# Export test utilities
__all__ = [
    'BeetFSTestCase',
    'RealMusicTestCase',
    'BeetsLibraryFixture',
    'create_synthetic_flac',
    'is_fuse_available',
    'is_ffmpeg_available',
    'is_flac_available',
    'BEETFS_ROOT',
    'REAL_MUSIC_PATH',
    'MOUNT_TIMEOUT',
]
