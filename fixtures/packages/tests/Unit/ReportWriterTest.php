<?php

namespace Tests\Unit;

use Papyrus\Pdf\Document;
use PHPUnit\Framework\TestCase;

class ReportWriterTest extends TestCase
{
    public function test_document_is_a_class(): void
    {
        $this->assertTrue(class_exists(Document::class));
    }
}
