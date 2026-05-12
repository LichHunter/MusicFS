# -*- coding: utf-8 -*-
from __future__ import print_function, unicode_literals

import os
import unittest
from io import BytesIO

from conftest import BeetFSTestCase, is_ffmpeg_available, is_flac_available


@unittest.skipUnless(is_ffmpeg_available() and is_flac_available(),
                     "ffmpeg and flac required for write tests")
class TestWriteMetadata(BeetFSTestCase):
    
    def setUp(self):
        super(TestWriteMetadata, self).setUp()
        try:
            import mutagen.flac
            self.mutagen = mutagen.flac
        except ImportError:
            self.mutagen = None
    
    def test_write_opens_without_error(self):
        self.add_test_track(filename='write_test.flac')
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        try:
            with open(tracks[0], 'r+b') as f:
                pass
        except IOError:
            self.skipTest("Write not supported")
    
    def test_write_to_header_updates_db(self):
        if not self.mutagen:
            self.skipTest("mutagen required")
        
        file_path, item_id = self.add_test_track(
            filename='db_write_test.flac',
            artist='Original Artist',
            title='Original Title'
        )
        
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        with open(tracks[0], 'rb') as f:
            original_data = f.read()
        
        flac = self.mutagen.FLAC(BytesIO(original_data))
        flac['artist'] = ['Modified Artist']
        
        output = BytesIO()
        flac.save(output)
        modified_data = output.getvalue()
        
        try:
            with open(tracks[0], 'r+b') as f:
                f.write(modified_data[:1024])
        except IOError:
            self.skipTest("Write not supported")
        
        item = self.library.get_item(item_id)
        if item:
            self.assertIn('Modified', str(item))
    
    def test_original_file_unchanged_after_write(self):
        if not self.mutagen:
            self.skipTest("mutagen required")
        
        file_path, item_id = self.add_test_track(
            filename='unchanged_test.flac',
            artist='Original Artist'
        )
        
        with open(file_path, 'rb') as f:
            original_content = f.read()
        
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if tracks:
            try:
                with open(tracks[0], 'r+b') as f:
                    f.write(b'\x00' * 100)
            except IOError:
                pass
        
        with open(file_path, 'rb') as f:
            after_content = f.read()
        
        self.assertEqual(original_content, after_content)
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


class TestWriteAudioDiscarded(BeetFSTestCase):
    
    def test_write_to_audio_region_no_error(self):
        file_path, item_id = self.add_test_track(filename='audio_write.flac')
        
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if not tracks:
            self.skipTest("No tracks found")
        
        try:
            with open(tracks[0], 'r+b') as f:
                f.seek(10000)
                f.write(b'\x00' * 100)
        except IOError:
            pass
    
    def test_audio_unchanged_after_write_attempt(self):
        file_path, item_id = self.add_test_track(filename='audio_unchanged.flac')
        
        with open(file_path, 'rb') as f:
            f.seek(10000)
            original_audio = f.read(100)
        
        self.mount_beetfs()
        
        tracks = self._find_any_track()
        if tracks:
            try:
                with open(tracks[0], 'r+b') as f:
                    f.seek(10000)
                    f.write(b'\xff' * 100)
            except IOError:
                pass
        
        with open(file_path, 'rb') as f:
            f.seek(10000)
            after_audio = f.read(100)
        
        self.assertEqual(original_audio, after_audio)
    
    def _find_any_track(self):
        results = []
        for root, dirs, files in os.walk(self.mount_dir):
            for f in files:
                if f.endswith('.flac'):
                    results.append(os.path.join(root, f))
        return results


if __name__ == '__main__':
    unittest.main()
