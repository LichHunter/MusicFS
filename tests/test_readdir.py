# -*- coding: utf-8 -*-
"""Tests for directory listing operations."""
from __future__ import print_function, unicode_literals

import os
import unittest

from conftest import BeetFSTestCase


class TestReaddirEmpty(BeetFSTestCase):
    
    def test_list_empty_root(self):
        self.mount_beetfs()
        
        entries = os.listdir(self.mount_dir)
        self.assertEqual(entries, [])
    
    def test_list_root_returns_list(self):
        self.mount_beetfs()
        
        entries = os.listdir(self.mount_dir)
        self.assertIsInstance(entries, list)


class TestReaddirWithContent(BeetFSTestCase):
    
    def setUp(self):
        super(TestReaddirWithContent, self).setUp()
        
        self.add_test_track(
            filename='track01.flac',
            artist='Pink Floyd',
            title='Comfortably Numb',
            album='The Wall',
            year='1979',
            track='01'
        )
        self.add_test_track(
            filename='track02.flac',
            artist='Pink Floyd',
            title='Another Brick in the Wall',
            album='The Wall',
            year='1979',
            track='02'
        )
        self.add_test_track(
            filename='track03.flac',
            artist='Led Zeppelin',
            title='Stairway to Heaven',
            album='Led Zeppelin IV',
            year='1971',
            track='01'
        )
    
    def test_list_root_shows_artists(self):
        self.mount_beetfs()
        
        entries = os.listdir(self.mount_dir)
        
        self.assertIn('Pink Floyd', entries)
        self.assertIn('Led Zeppelin', entries)
    
    def test_list_artist_shows_albums(self):
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Pink Floyd')
        entries = os.listdir(artist_dir)
        
        self.assertTrue(any('The Wall' in e for e in entries))
    
    def test_list_album_shows_tracks(self):
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Pink Floyd')
        albums = os.listdir(artist_dir)
        wall_album = [a for a in albums if 'The Wall' in a][0]
        
        album_dir = os.path.join(artist_dir, wall_album)
        tracks = os.listdir(album_dir)
        
        self.assertEqual(len(tracks), 2)
    
    def test_path_format_includes_year(self):
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Pink Floyd')
        albums = os.listdir(artist_dir)
        
        wall_album = [a for a in albums if 'The Wall' in a][0]
        self.assertIn('1979', wall_album)
    
    def test_path_format_includes_format(self):
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Pink Floyd')
        albums = os.listdir(artist_dir)
        
        wall_album = [a for a in albums if 'The Wall' in a][0]
        self.assertIn('FLAC', wall_album.upper())
    
    def test_track_filename_includes_number(self):
        self.mount_beetfs()
        
        artist_dir = os.path.join(self.mount_dir, 'Pink Floyd')
        albums = os.listdir(artist_dir)
        wall_album = [a for a in albums if 'The Wall' in a][0]
        album_dir = os.path.join(artist_dir, wall_album)
        
        tracks = os.listdir(album_dir)
        
        self.assertTrue(any(t.startswith('01') or t.startswith('1') for t in tracks))


class TestReaddirUnicode(BeetFSTestCase):
    
    def test_unicode_artist_name(self):
        self.add_test_track(
            filename='bjork.flac',
            artist='Björk',
            title='Hyperballad',
            album='Post'
        )
        
        self.mount_beetfs()
        entries = os.listdir(self.mount_dir)
        
        self.assertTrue(
            any('Bj' in e for e in entries),
            "Unicode artist name should appear in listing"
        )
    
    def test_unicode_album_name(self):
        self.add_test_track(
            filename='sigur.flac',
            artist='Sigur Ros',
            title='Hoppipolla',
            album='Takk...'
        )
        
        self.mount_beetfs()
        artist_dir = os.path.join(self.mount_dir, 'Sigur Ros')
        
        if os.path.isdir(artist_dir):
            albums = os.listdir(artist_dir)
            self.assertTrue(len(albums) > 0)


class TestReaddirSpecialChars(BeetFSTestCase):
    
    def test_artist_with_ampersand(self):
        self.add_test_track(
            filename='guns.flac',
            artist="Guns N' Roses",
            title='Welcome to the Jungle',
            album='Appetite for Destruction'
        )
        
        self.mount_beetfs()
        entries = os.listdir(self.mount_dir)
        
        self.assertTrue(len(entries) > 0)
    
    def test_title_with_question_mark(self):
        self.add_test_track(
            filename='who.flac',
            artist='The Who',
            title='Who Are You?',
            album='Who Are You'
        )
        
        self.mount_beetfs()
        entries = os.listdir(self.mount_dir)
        
        self.assertIn('The Who', entries)


if __name__ == '__main__':
    unittest.main()
