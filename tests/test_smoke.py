# -*- coding: utf-8 -*-
"""Smoke tests for beetfs mount/unmount lifecycle."""
from __future__ import print_function, unicode_literals

import os
import unittest

from conftest import BeetFSTestCase, is_fuse_available


class TestMountLifecycle(BeetFSTestCase):
    
    def test_fuse_available(self):
        self.assertTrue(is_fuse_available())
    
    def test_mount_empty_library(self):
        self.mount_beetfs()
        self.assertTrue(os.path.ismount(self.mount_dir))
    
    def test_unmount_clean(self):
        self.mount_beetfs()
        self.assertTrue(os.path.ismount(self.mount_dir))
        
        self._unmount()
        self._wait_for_process_exit(5)
        
        self.assertFalse(os.path.ismount(self.mount_dir))
    
    def test_mount_with_single_track(self):
        self.add_test_track(
            filename='track01.flac',
            artist='Smoke Test Artist',
            title='Smoke Test Track',
            album='Smoke Test Album'
        )
        
        self.mount_beetfs()
        self.assertTrue(os.path.ismount(self.mount_dir))
    
    def test_mount_directory_accessible(self):
        self.mount_beetfs()
        
        entries = os.listdir(self.mount_dir)
        self.assertIsInstance(entries, list)
    
    def test_temp_directory_created(self):
        self.assertTrue(os.path.isdir(self.temp_dir))
        self.assertTrue(os.path.isdir(self.mount_dir))
    
    def test_library_fixture_created(self):
        self.assertTrue(os.path.isfile(self.library.db_path))
        self.assertTrue(os.path.isdir(self.library.music_dir))


class TestMountWithContent(BeetFSTestCase):
    
    def test_mount_with_multiple_tracks(self):
        for i in range(3):
            self.add_test_track(
                filename='track{:02d}.flac'.format(i + 1),
                artist='Test Artist',
                title='Track {}'.format(i + 1),
                album='Test Album',
                track=str(i + 1)
            )
        
        self.mount_beetfs()
        self.assertTrue(os.path.ismount(self.mount_dir))
    
    def test_mount_with_unicode_metadata(self):
        self.add_test_track(
            filename='unicode_track.flac',
            artist='Tëst Ärtîst',
            title='Ünïcödé Träck',
            album='Spëcîäl Chäräctërs'
        )
        
        self.mount_beetfs()
        self.assertTrue(os.path.ismount(self.mount_dir))


if __name__ == '__main__':
    unittest.main()
