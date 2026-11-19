<?php

namespace App\Services;

class InvoicePrinter
{
    public function print(string $view): string
    {
        return \PDF::loadView($view)->output();
    }
}
