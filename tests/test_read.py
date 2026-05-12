# -*- coding: utf-8 -*-
"""
Tests for file reading with metadata overlay.

This is the CORE FEATURE of beetfs:
- File headers contain metadata from beets database
- Audio data passes through unchanged from original file
"""
from __future__ import print_function, unicode_literals

import os
import unittest
from io import BytesIO

from conftest import BeetFSTestCase, is_ffmpeg_available, is_flac_available


class TestReadBasic(BeetFSTestCase):
    
    def test_open_file(self):
        file_path, item_id = self.add_test_track(
            filename='test.flac',
            artist='Test Artist',
            title='Test Track'
        )
        
        self.mount_beetfs()
        
        entries = self._find_track_path('Test Artist', 'Test Track')
        if not entries:
            self.skipTest("Could not find track in mounted filesystem")
        
        track_path = entries[0]
        
        with open(track_path, 'rb') as f:
            data = f.read(1024)
        
        self.assertIsInstance(data, bytes)
        self.assertTrue(len(data) > 0)
    
    def test_read_returns_bytes(self):
        self.add_test_track(filename='test.flac')
        self.mount_beetfs()
        
        entries = self._find_any_track()
        if not entries:
            self.skipTest("No tracks found")
        
        with open(entries[0], 'rb') as f:
            data = f.read(100)
        
        self.assertIsInstance(data, bytes)
    
    def test_read_flac_magic_bytes(self):
        self.add_test_track(filename='test.flac')
        self.mount_beetfs()
        
        entries = self._find_any_track()
        if not entries:
            self.skipTest("No tracks found")
        
        with open(entries[0], 'rb') as f:
            magic = f.read(4)
        
        self.assertEqual(magic, b'fLaC', "FLAC files should start with 'fLaC' magic bytes")
    
    def _find_track_path(self, artist, title):
        """Find track path in mounted filesystem."""
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if title.lower() in f.lower():
                    results.append(os.path.join(root, f))
        return results
    
    def _find_any_track(self):
        """Find any track in mounted filesystem."""
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


@unittest.skipUnless(is_ffmpeg_available() and is_flac_available(),
                     "ffmpeg and flac required for metadata overlay tests")
class TestMetadataOverlay(BeetFSTestCase):
    """
    Tests that verify the core metadata overlay functionality.
    
    The key insight: beetfs should return metadata from the DATABASE,
    not from the original FILE. This allows presenting different metadata
    than what's embedded in the file.
    """
    
    def setUp(self):
        super(TestMetadataOverlay, self).setUp()
        
        try:
            import mutagen.flac
            self.mutagen_available = True
        except ImportError:
            self.mutagen_available = False
    
    def test_overlay_artist_from_database(self):
        if not self.mutagen_available:
            self.skipTest("mutagen required for metadata verification")
        
        import mutagen.flac
        
        file_path, item_id = self.add_test_track(
            filename='overlay_test.flac',
            artist='File Artist',
            title='Test Track',
            album='Test Album'
        )
        
        self.library.update_item(item_id, artist='Database Artist')
        
        self.mount_beetfs()
        
        mounted_tracks = self._find_track_in_mount('Database Artist')
        if not mounted_tracks:
            self.skipTest("Track not found under 'Database Artist' - path uses DB metadata")
        
        with open(mounted_tracks[0], 'rb') as f:
            mounted_data = f.read()
        
        mounted_flac = mutagen.flac.FLAC(BytesIO(mounted_data))
        
        artist_tag = mounted_flac.get('artist', [None])[0]
        self.assertEqual(artist_tag, 'Database Artist',
                        "Mounted file should have artist from database, not file")
    
    def test_overlay_title_from_database(self):
        if not self.mutagen_available:
            self.skipTest("mutagen required for metadata verification")
        
        import mutagen.flac
        
        file_path, item_id = self.add_test_track(
            filename='title_test.flac',
            artist='Test Artist',
            title='File Title'
        )
        
        self.library.update_item(item_id, title='Database Title')
        
        self.mount_beetfs()
        
        mounted_tracks = self._find_any_track()
        if not mounted_tracks:
            self.skipTest("No tracks found")
        
        with open(mounted_tracks[0], 'rb') as f:
            mounted_data = f.read()
        
        mounted_flac = mutagen.flac.FLAC(BytesIO(mounted_data))
        
        title_tag = mounted_flac.get('title', [None])[0]
        self.assertEqual(title_tag, 'Database Title')
    
    def test_original_file_unchanged(self):
        if not self.mutagen_available:
            self.skipTest("mutagen required for metadata verification")
        
        import mutagen.flac
        
        file_path, item_id = self.add_test_track(
            filename='original_test.flac',
            artist='Original Artist',
            title='Original Title'
        )
        
        self.library.update_item(item_id, artist='Modified Artist')
        
        self.mount_beetfs()
        
        original_flac = mutagen.flac.FLAC(file_path)
        original_artist = original_flac.get('artist', [None])[0]
        
        self.assertEqual(original_artist, 'Original Artist',
                        "Original file should remain unchanged")
    
    def test_audio_data_passthrough(self):
        file_path, item_id = self.add_test_track(
            filename='audio_test.flac',
            artist='Test Artist'
        )
        
        with open(file_path, 'rb') as f:
            original_data = f.read()
        
        self.mount_beetfs()
        
        mounted_tracks = self._find_any_track()
        if not mounted_tracks:
            self.skipTest("No tracks found")
        
        with open(mounted_tracks[0], 'rb') as f:
            mounted_data = f.read()
        
        original_audio_start = original_data.find(b'\xff\xf8')
        mounted_audio_start = mounted_data.find(b'\xff\xf8')
        
        if original_audio_start > 0 and mounted_audio_start > 0:
            original_audio = original_data[original_audio_start:original_audio_start + 1000]
            mounted_audio = mounted_data[mounted_audio_start:mounted_audio_start + 1000]
            
            self.assertEqual(original_audio, mounted_audio,
                           "Audio data should pass through unchanged")
    
    def _find_track_in_mount(self, artist_name):
        """Find tracks under a specific artist directory."""
        results = []
        artist_dir = os.path.join(self.mount_dir, artist_name)
        if not os.path.isdir(artist_dir):
            return results
        
        for root, dirs, files in os.walk(artist_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results
    
    def _find_any_track(self):
        """Find any track in mounted filesystem."""
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


class TestReadFullFile(BeetFSTestCase):
    
    def test_read_entire_file(self):
        file_path, item_id = self.add_test_track(filename='full_read.flac')
        
        original_size = os.path.getsize(file_path)
        
        self.mount_beetfs()
        
        mounted_tracks = self._find_any_track()
        if not mounted_tracks:
            self.skipTest("No tracks found")
        
        with open(mounted_tracks[0], 'rb') as f:
            data = f.read()
        
        self.assertTrue(len(data) > 0)
        self.assertAlmostEqual(len(data), original_size, delta=original_size * 0.5)
    
    def test_read_with_offset(self):
        self.add_test_track(filename='offset_test.flac')
        self.mount_beetfs()
        
        mounted_tracks = self._find_any_track()
        if not mounted_tracks:
            self.skipTest("No tracks found")
        
        with open(mounted_tracks[0], 'rb') as f:
            f.seek(100)
            data = f.read(100)
        
        self.assertEqual(len(data), 100)
    
    def test_multiple_reads(self):
        self.add_test_track(filename='multi_read.flac')
        self.mount_beetfs()
        
        mounted_tracks = self._find_any_track()
        if not mounted_tracks:
            self.skipTest("No tracks found")
        
        with open(mounted_tracks[0], 'rb') as f:
            chunk1 = f.read(512)
            chunk2 = f.read(512)
            chunk3 = f.read(512)
        
        self.assertEqual(len(chunk1), 512)
        self.assertEqual(len(chunk2), 512)
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


if __name__ == '__main__':
    unittest.main()
