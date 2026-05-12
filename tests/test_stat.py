# -*- coding: utf-8 -*-
from __future__ import print_function, unicode_literals

import os
import stat
import unittest

from conftest import BeetFSTestCase


class TestStatFile(BeetFSTestCase):
    
    def setUp(self):
        super(TestStatFile, self).setUp()
        self.file_path, self.item_id = self.add_test_track(
            filename='stat_test.flac',
            artist='Test Artist',
            title='Test Track',
            album='Test Album'
        )
    
    def test_stat_returns_stat_result(self):
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        result = os.stat(tracks[0])
        self.assertTrue(hasattr(result, 'st_mode'))
        self.assertTrue(hasattr(result, 'st_size'))
    
    def test_stat_file_mode(self):
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        result = os.stat(tracks[0])
        self.assertTrue(stat.S_ISREG(result.st_mode))
    
    def test_stat_file_size_positive(self):
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        result = os.stat(tracks[0])
        self.assertGreater(result.st_size, 0)
    
    def test_stat_file_size_matches_original(self):
        original_size = os.path.getsize(self.file_path)
        
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        result = os.stat(tracks[0])
        self.assertAlmostEqual(result.st_size, original_size, delta=original_size * 0.5)
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


class TestStatDirectory(BeetFSTestCase):
    
    def test_stat_root(self):
        self.mount_beetfs()
        
        result = os.stat(self.mount_dir)
        self.assertTrue(stat.S_ISDIR(result.st_mode))
    
    def test_stat_artist_directory(self):
        self.add_test_track(
            filename='test.flac',
            artist='Stat Test Artist'
        )
        
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Stat Test Artist')
        if os.path.isdir(artist_dir):
            result = os.stat(artist_dir)
            self.assertTrue(stat.S_ISDIR(result.st_mode))


class TestStatfs(BeetFSTestCase):
    
    def test_statvfs_returns_result(self):
        self.mount_beetfs()
        
        result = os.statvfs(self.mount_dir)
        self.assertTrue(hasattr(result, 'f_bsize'))
        self.assertTrue(hasattr(result, 'f_blocks'))


class TestAccess(BeetFSTestCase):
    
    def test_access_root_readable(self):
        self.mount_beetfs()
        
        self.assertTrue(os.access(self.mount_dir, os.R_OK))
    
    def test_access_root_executable(self):
        self.mount_beetfs()
        
        self.assertTrue(os.access(self.mount_dir, os.X_OK))
    
    def test_access_file_readable(self):
        self.add_test_track(filename='access_test.flac')
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        self.assertTrue(os.access(tracks[0], os.R_OK))
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


if __name__ == '__main__':
    unittest.main()
