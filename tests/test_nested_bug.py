# -*- coding: utf-8 -*-
"""
Tests to verify the nested methods bug in beetFs.py.

Oracle discovered that lines 758-1144 are indented inside access() method,
making readdir, open, read, write, etc. unreachable as class methods.
These tests will fail until the bug is fixed.
"""
from __future__ import print_function, unicode_literals

import os
import sys
import unittest

from conftest import BEETFS_ROOT

sys.path.insert(0, os.path.join(BEETFS_ROOT, 'beetsplug'))


class TestNestedMethodsBug(unittest.TestCase):
    """Verify FUSE methods exist at class level (not nested inside access)."""
    
    @classmethod
    def setUpClass(cls):
        try:
            from beetFs import beetFileSystem
            cls.fs_class = beetFileSystem
        except ImportError as e:
            raise unittest.SkipTest("Cannot import beetFs: {}".format(e))
    
    def test_readdir_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'readdir'),
            "readdir should be a class method, not nested inside access()"
        )
    
    def test_open_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'open'),
            "open should be a class method, not nested inside access()"
        )
    
    def test_read_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'read'),
            "read should be a class method, not nested inside access()"
        )
    
    def test_write_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'write'),
            "write should be a class method, not nested inside access()"
        )
    
    def test_release_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'release'),
            "release should be a class method, not nested inside access()"
        )
    
    def test_opendir_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'opendir'),
            "opendir should be a class method, not nested inside access()"
        )
    
    def test_releasedir_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'releasedir'),
            "releasedir should be a class method, not nested inside access()"
        )
    
    def test_mkdir_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'mkdir'),
            "mkdir should be a class method, not nested inside access()"
        )
    
    def test_unlink_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'unlink'),
            "unlink should be a class method, not nested inside access()"
        )
    
    def test_truncate_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'truncate'),
            "truncate should be a class method, not nested inside access()"
        )
    
    def test_fgetattr_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'fgetattr'),
            "fgetattr should be a class method, not nested inside access()"
        )
    
    def test_flush_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'flush'),
            "flush should be a class method, not nested inside access()"
        )
    
    def test_fsync_is_class_method(self):
        self.assertTrue(
            hasattr(self.fs_class, 'fsync'),
            "fsync should be a class method, not nested inside access()"
        )


class TestMethodsCallable(unittest.TestCase):
    """Verify methods can be called (not just exist)."""
    
    @classmethod
    def setUpClass(cls):
        try:
            from beetFs import beetFileSystem
            cls.fs_class = beetFileSystem
        except ImportError as e:
            raise unittest.SkipTest("Cannot import beetFs: {}".format(e))
    
    def test_readdir_callable(self):
        method = getattr(self.fs_class, 'readdir', None)
        if method is None:
            self.skipTest("readdir not found - nested bug not fixed yet")
        self.assertTrue(callable(method))
    
    def test_open_callable(self):
        method = getattr(self.fs_class, 'open', None)
        if method is None:
            self.skipTest("open not found - nested bug not fixed yet")
        self.assertTrue(callable(method))
    
    def test_read_callable(self):
        method = getattr(self.fs_class, 'read', None)
        if method is None:
            self.skipTest("read not found - nested bug not fixed yet")
        self.assertTrue(callable(method))


if __name__ == '__main__':
    unittest.main()
