# -*- coding: utf-8 -*-
from __future__ import print_function, unicode_literals

import errno
import os
import unittest

from conftest import BeetFSTestCase


class TestENOENT(BeetFSTestCase):
    
    def test_stat_nonexistent_file(self):
        self.mount_beetfs()
        
        nonexistent = os.path.join(self.mount_dir, 'does_not_exist.flac')
        
        with self.assertRaises(OSError) as ctx:
            os.stat(nonexistent)
        
        self.assertEqual(ctx.exception.errno, errno.ENOENT)
    
    def test_open_nonexistent_file(self):
        self.mount_beetfs()
        
        nonexistent = os.path.join(self.mount_dir, 'does_not_exist.flac')
        
        with self.assertRaises(IOError) as ctx:
            open(nonexistent, 'rb')
        
        self.assertEqual(ctx.exception.errno, errno.ENOENT)
    
    def test_listdir_nonexistent_directory(self):
        self.mount_beetfs()
        
        nonexistent = os.path.join(self.mount_dir, 'Nonexistent Artist')
        
        with self.assertRaises(OSError) as ctx:
            os.listdir(nonexistent)
        
        self.assertEqual(ctx.exception.errno, errno.ENOENT)
    
    def test_stat_deeply_nested_nonexistent(self):
        self.mount_beetfs()
        
        nonexistent = os.path.join(
            self.mount_dir, 'Artist', 'Album', 'Track', 'Nested', 'Path'
        )
        
        with self.assertRaises(OSError) as ctx:
            os.stat(nonexistent)
        
        self.assertEqual(ctx.exception.errno, errno.ENOENT)


class TestEOPNOTSUPP(BeetFSTestCase):
    
    def test_mkdir_not_supported(self):
        self.mount_beetfs()
        
        new_dir = os.path.join(self.mount_dir, 'new_directory')
        
        with self.assertRaises(OSError) as ctx:
            os.mkdir(new_dir)
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS])
    
    def test_unlink_not_supported(self):
        self.add_test_track(filename='delete_test.flac')
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        with self.assertRaises(OSError) as ctx:
            os.unlink(tracks[0])
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS])
    
    def test_rmdir_not_supported(self):
        self.add_test_track(filename='rmdir_test.flac', artist='Rmdir Artist')
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Rmdir Artist')
        if not os.path.isdir(artist_dir):
            self.skipTest("Artist directory not found")
        
        with self.assertRaises(OSError) as ctx:
            os.rmdir(artist_dir)
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS, errno.ENOTEMPTY])
    
    def test_rename_not_supported(self):
        self.add_test_track(filename='rename_test.flac')
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        new_path = tracks[0] + '.renamed'
        
        with self.assertRaises(OSError) as ctx:
            os.rename(tracks[0], new_path)
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS])
    
    def test_symlink_not_supported(self):
        self.mount_beetfs()
        
        link_path = os.path.join(self.mount_dir, 'test_link')
        
        with self.assertRaises(OSError) as ctx:
            os.symlink('/tmp', link_path)
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS])
    
    def test_chmod_not_supported(self):
        self.add_test_track(filename='chmod_test.flac')
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        with self.assertRaises(OSError) as ctx:
            os.chmod(tracks[0], 0o777)
        
        self.assertIn(ctx.exception.errno, [errno.EOPNOTSUPP, errno.EACCES, errno.EROFS])
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


class TestEdgeCaseErrors(BeetFSTestCase):
    
    def test_access_empty_path(self):
        self.mount_beetfs()
        
        with self.assertRaises((OSError, IOError)):
            os.stat(os.path.join(self.mount_dir, ''))
    
    def test_open_directory_as_file(self):
        self.add_test_track(filename='test.flac', artist='Dir Artist')
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Dir Artist')
        if os.path.isdir(artist_dir):
            with self.assertRaises((OSError, IOError)):
                with open(artist_dir, 'rb') as f:
                    f.read()


if __name__ == '__main__':
    unittest.main()
