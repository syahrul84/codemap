<?php

namespace App\Services;

use Papyrus\Pdf\Document;

class ReportWriter
{
    public function pdf(string $html): string
    {
        return (new Document())->html($html)->render();
    }

    public function sheet(array $rows): string
    {
        return \Sheetgen\SheetGen::fromArray($rows)->toString();
    }
}
